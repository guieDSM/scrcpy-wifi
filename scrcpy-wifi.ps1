<#
  scrcpy-wifi: find an Android phone on the local network via mDNS,
  pair on first run (Android 11+ wireless debugging), connect, start scrcpy.

  Usage:
    .\scrcpy-wifi.ps1                 # find, connect, start scrcpy
    .\scrcpy-wifi.ps1 -Pair           # force pairing with a code
    .\scrcpy-wifi.ps1 -- --no-audio   # everything after -- goes to scrcpy
#>
param(
    [switch]$Pair,
    [string]$Ip,
    [int]$TimeoutSec = 20,
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$ScrcpyArgs
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$bin  = Join-Path $root 'scrcpy'
$adb  = Join-Path $bin 'adb.exe'
$scrcpy = Join-Path $bin 'scrcpy.exe'
$stateFile = Join-Path $root 'last-device.txt'

if (-not (Test-Path $adb) -or -not (Test-Path $scrcpy)) {
    Write-Host "Не найден scrcpy в $bin. Распакуйте scrcpy-win32-v*.zip в эту папку (см. README)." -ForegroundColor Red
    exit 1
}

function Invoke-Adb { & $adb @args 2>&1 | ForEach-Object { "$_" } }

function Get-MdnsServices([string]$type) {
    # lines: <name>\t<type>\t<ip:port>
    Invoke-Adb mdns services | ForEach-Object {
        $f = $_ -split "\s+"
        if ($f.Count -ge 3 -and $f[1] -like "$type*") {
            [pscustomobject]@{ Name = $f[0]; Type = $f[1]; Addr = $f[2] }
        }
    } | Where-Object { -not $Ip -or $_.Addr -like "$Ip`:*" }
}

function Get-WifiDevice {
    Invoke-Adb devices | Select-Object -Skip 1 | ForEach-Object {
        $f = $_ -split "\s+"
        if ($f.Count -ge 2 -and $f[1] -eq 'device' -and ($f[0] -match ':\d+$' -or $f[0] -like '*_adb-tls-connect*')) { $f[0] }
    } | Select-Object -First 1
}

function Wait-For([scriptblock]$probe, [int]$sec, [string]$what) {
    $deadline = (Get-Date).AddSeconds($sec)
    while ((Get-Date) -lt $deadline) {
        $r = & $probe
        if ($r) { return $r }
        Start-Sleep -Milliseconds 700
    }
    Write-Host "Не дождался: $what" -ForegroundColor Yellow
    return $null
}

function Connect-Device {
    $svc = Wait-For { Get-MdnsServices '_adb-tls-connect' | Select-Object -First 1 } $TimeoutSec 'телефон в сети (_adb-tls-connect)'
    if (-not $svc) { return $null }
    Write-Host "Найден: $($svc.Name) $($svc.Addr)"
    $out = Invoke-Adb connect $svc.Addr
    Write-Host ($out -join "`n")
    return (Wait-For { Get-WifiDevice } 5 'подключение')
}

function Start-Pairing {
    Write-Host ""
    Write-Host "Сопряжение. На телефоне: Параметры > Для разработчиков > Отладка по Wi-Fi >" -ForegroundColor Cyan
    Write-Host "  «Подключить устройство с помощью кода» и не закрывайте окно с кодом." -ForegroundColor Cyan
    $svc = Wait-For { Get-MdnsServices '_adb-tls-pairing' | Select-Object -First 1 } 120 'окно сопряжения на телефоне'
    if ($svc) {
        $addr = $svc.Addr
        Write-Host "Окно сопряжения найдено: $addr"
    } else {
        $addr = Read-Host "Введите IP:порт из окна сопряжения"
    }
    $code = Read-Host "Код сопряжения (6 цифр)"
    $out = Invoke-Adb pair $addr $code
    Write-Host ($out -join "`n")
    if (-not ($out -match 'Successfully paired')) { Write-Host "Сопряжение не удалось." -ForegroundColor Red; exit 2 }
}

Invoke-Adb start-server | Out-Null

$serial = Get-WifiDevice
if (-not $serial -and -not $Pair) { $serial = Connect-Device }
if (-not $serial) {
    Start-Pairing
    $serial = Connect-Device
}
if (-not $serial) {
    Write-Host "Не удалось подключиться. Проверьте, что телефон и ПК в одной Wi-Fi сети и отладка по Wi-Fi включена." -ForegroundColor Red
    exit 3
}

Set-Content -Path $stateFile -Value $serial -Encoding utf8
Write-Host "Подключено: $serial" -ForegroundColor Green

# Wi-Fi friendly defaults; any extra arguments override/extend them.
$defaults = @('--video-bit-rate=8M', '--max-size=1920', '--max-fps=60')
& $scrcpy -s $serial @defaults @ScrcpyArgs
