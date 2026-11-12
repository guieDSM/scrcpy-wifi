<#
  scrcpy-wifi: screen of an Android phone on a Windows PC over Wi-Fi.
  First run downloads scrcpy (official GitHub release, SHA-256 checked),
  finds the phone via mDNS, pairs with a code once, then connects and starts scrcpy.

  .\scrcpy-wifi.ps1                       # find, connect, start
  .\scrcpy-wifi.ps1 -Pair                 # pair again
  .\scrcpy-wifi.ps1 -Ip 192.168.1.20      # only this phone
  .\scrcpy-wifi.ps1 --no-audio            # other arguments go to scrcpy
#>
param(
    [switch]$Pair,
    [string]$Ip,
    [int]$TimeoutSec = 15,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$ScrcpyArgs
)

$ErrorActionPreference = 'Stop'
try { [Console]::OutputEncoding = [Text.Encoding]::UTF8 } catch {}
$ProgressPreference = 'SilentlyContinue'

$root   = Split-Path -Parent $MyInvocation.MyCommand.Path
$bin    = Join-Path $root 'scrcpy'
$adb    = Join-Path $bin 'adb.exe'
$scrcpy = Join-Path $bin 'scrcpy.exe'

# Pinned official release: https://github.com/Genymobile/scrcpy/releases/tag/v5.0.1
$ScrcpyVersion = 'v5.0.1'
$ScrcpyBuilds = @{
    'win64'    = 'b12a2c4ee8be317422451fc7dcf8ee20a71b5ea7ef9ad73ddd825a25316227a5'
    'win32'    = '0cb65ebf1bce892fa4d1502d699af0c7a1a7f24d7febcb9ef72894e3f23fc2ad'
    'winarm64' = '16c3fc2068df64946f670e3504b4752073ebae47a416a45b46e79a5ab19c689b'
}

function Say([string]$text, [string]$color = 'Gray') { Write-Host $text -ForegroundColor $color }
function Step([string]$text) { Write-Host ""; Write-Host "==> $text" -ForegroundColor Cyan }
function Fail([string]$text, [int]$code) { Write-Host ""; Write-Host $text -ForegroundColor Red; exit $code }

function Install-Scrcpy {
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    $build = switch ($arch) { 'ARM64' { 'winarm64' } 'AMD64' { 'win64' } default { 'win32' } }
    $name = "scrcpy-$build-$ScrcpyVersion"
    $url  = "https://github.com/Genymobile/scrcpy/releases/download/$ScrcpyVersion/$name.zip"

    Step "Первый запуск: скачиваю scrcpy $ScrcpyVersion ($build), ~11 МБ"
    Say  $url
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("scrcpy-wifi-" + [guid]::NewGuid())
    New-Item -ItemType Directory $tmp | Out-Null
    try {
        $zip = Join-Path $tmp "$name.zip"
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        try { Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing }
        catch { Fail "Не удалось скачать scrcpy: $($_.Exception.Message)`nСкачайте $name.zip вручную и распакуйте содержимое в папку $bin" 1 }

        $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
        if ($hash -ne $ScrcpyBuilds[$build]) { Fail "Контрольная сумма архива не совпала, файл не использую.`nожидалось $($ScrcpyBuilds[$build])`nполучено  $hash" 1 }

        Expand-Archive -LiteralPath $zip -DestinationPath $tmp -Force
        $src = Join-Path $tmp $name
        if (-not (Test-Path $bin)) { New-Item -ItemType Directory $bin | Out-Null }
        Get-ChildItem $src | Move-Item -Destination $bin -Force
        Say "scrcpy установлен в $bin" Green
    } finally {
        Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }
}

function Invoke-Adb {
    # adb prints normal status to stderr; in PowerShell 5.1 that must not abort the script
    $ErrorActionPreference = 'Continue'
    & $adb @args 2>&1 | ForEach-Object { "$_" }
}

function Get-MdnsServices([string]$type) {
    # lines: <name>  <type>  <ip:port>
    Invoke-Adb mdns services | ForEach-Object {
        $f = $_ -split '\s+'
        if ($f.Count -ge 3 -and $f[1] -like "$type*") {
            [pscustomobject]@{ Name = $f[0]; Type = $f[1]; Addr = $f[2] }
        }
    } | Where-Object { -not $Ip -or $_.Addr -like "$Ip`:*" }
}

function Get-WifiDevice {
    Invoke-Adb devices | Select-Object -Skip 1 | ForEach-Object {
        $f = $_ -split '\s+'
        if ($f.Count -ge 2 -and $f[1] -eq 'device' -and ($f[0] -match ':\d+$' -or $f[0] -like '*_adb-tls-connect*')) {
            if (-not $Ip -or $f[0] -like "$Ip`:*" -or $f[0] -like '*_adb-tls-connect*') { $f[0] }
        }
    } | Select-Object -First 1
}

function Wait-For([scriptblock]$probe, [int]$sec) {
    $deadline = (Get-Date).AddSeconds($sec)
    while ((Get-Date) -lt $deadline) {
        $r = & $probe
        if ($r) { return $r }
        Start-Sleep -Milliseconds 700
    }
    return $null
}

function Connect-Device {
    Step "Ищу телефон в сети"
    $svc = Wait-For { Get-MdnsServices '_adb-tls-connect' | Select-Object -First 1 } $TimeoutSec
    if ($svc) {
        Say "Найден: $($svc.Addr)"
        Invoke-Adb connect $svc.Addr | Out-Null
    } else {
        Say "Автоматически не нашёлся." Yellow
        Say "На телефоне в «Отладке по Wi-Fi» вверху написан «IP-адрес и порт», например 192.168.1.20:41234."
        $addr = (Read-Host "Введите его (или Enter, чтобы пропустить)").Trim()
        if (-not $addr) { return $null }
        Say ((Invoke-Adb connect $addr) -join "`n")
    }
    return (Wait-For { Get-WifiDevice } 6)
}

function Start-Pairing {
    Step "Сопряжение (нужно один раз)"
    Say "На телефоне: Настройки > Система > Для разработчиков > Отладка по Wi-Fi >"
    Say "«Подключение с помощью кода подключения». Окно с кодом не закрывайте." Yellow
    Say "Жду это окно на телефоне..."
    $svc = Wait-For { Get-MdnsServices '_adb-tls-pairing' | Select-Object -First 1 } 120
    if ($svc) {
        $addr = $svc.Addr
        Say "Окно сопряжения найдено: $addr" Green
    } else {
        Say "Окно сопряжения не нашлось автоматически." Yellow
        $addr = (Read-Host "Введите «IP-адрес и порт» из окна с кодом").Trim()
    }
    for ($i = 1; $i -le 3; $i++) {
        $code = (Read-Host "Код из окна (6 цифр)").Trim()
        if ($code -notmatch '^\d{6}$') { Say "Нужно ровно 6 цифр." Yellow; continue }
        $out = Invoke-Adb pair $addr $code
        if ($out -match 'Successfully paired') { Say "Сопряжено." Green; return }
        Say ($out -join "`n") Yellow
    }
    Fail "Сопряжение не удалось. Запустите «Сопрячь заново.cmd» и попробуйте ещё раз." 2
}

# --- main ---
if (-not (Test-Path $adb) -or -not (Test-Path $scrcpy)) { Install-Scrcpy }

Invoke-Adb start-server | Out-Null

$serial = $null
if (-not $Pair) {
    $serial = Get-WifiDevice
    if (-not $serial) { $serial = Connect-Device }
}
if (-not $serial) {
    Start-Pairing
    $serial = Connect-Device
}
if (-not $serial) {
    Fail ("Не удалось подключиться. Проверьте:`n" +
          "  - телефон и ПК в одной Wi-Fi сети (не гостевой);`n" +
          "  - экран телефона включён, «Отладка по Wi-Fi» включена;`n" +
          "  - VPN на ПК выключен или пропускает локальную сеть.") 3
}

Step "Подключено: $serial"
Say "Окно с экраном телефона сейчас откроется. Закройте его, чтобы отключиться."

# Wi-Fi friendly defaults; extra arguments are appended and win over these.
$defaults = @('--video-bit-rate=8M', '--max-size=1920', '--max-fps=60')
& $scrcpy -s $serial @defaults @ScrcpyArgs
exit $LASTEXITCODE
