# Smowauncher extension: qBittorrent Web UI API (v2). Prints {"items": [...]} as JSON.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$base = $env:SMOW_SETTING_URL.TrimEnd('/')
$query = $env:SMOW_QUERY
$action = $env:SMOW_ACTION

function Out-Result($items, $message = '') {
    [Console]::Out.Write((ConvertTo-Json -InputObject ([pscustomobject]@{ items = @($items); message = $message }) -Depth 6 -Compress))
    exit 0
}

function Format-Size([double]$bytes) {
    $units = 'B', 'KB', 'MB', 'GB', 'TB'
    $i = 0
    while ($bytes -ge 1024 -and $i -lt $units.Count - 1) { $bytes /= 1024; $i++ }
    if ($i -eq 0) { return "$([int]$bytes) B" }
    return ('{0:0.0} {1}' -f $bytes, $units[$i])
}

function Format-Eta([long]$seconds) {
    if ($seconds -ge 8640000 -or $seconds -lt 0) { return '' }
    $t = [TimeSpan]::FromSeconds($seconds)
    if ($t.TotalDays -ge 1) { return '{0}d {1}h' -f [int][Math]::Floor($t.TotalDays), $t.Hours }
    if ($t.TotalHours -ge 1) { return '{0}h {1}m' -f [int][Math]::Floor($t.TotalHours), $t.Minutes }
    if ($t.TotalMinutes -ge 1) { return '{0} min' -f [int][Math]::Floor($t.TotalMinutes) }
    return "$($t.Seconds) s"
}

$session = New-Object Microsoft.PowerShell.Commands.WebRequestSession
$headers = @{ Referer = $base }
try {
    if ($env:SMOW_SECRET_PASSWORD) {
        $login = Invoke-WebRequest "$base/api/v2/auth/login" -Method Post -WebSession $session -Headers $headers -UseBasicParsing `
            -Body @{ username = $env:SMOW_SETTING_USERNAME; password = $env:SMOW_SECRET_PASSWORD }
        if ($login.Content -notmatch 'Ok') {
            Out-Result @(@{ title = 'qBittorrent rejected the login'; subtitle = 'Check the username in extension.toml and the password in Settings → Extensions'; icon = [string][char]0xE72E })
        }
    }

    $message = ''
    if ($action -match '^(pause|resume):(.+)$') {
        # qBittorrent 5 renamed pause/resume to stop/start.
        $new = @{ pause = 'stop'; resume = 'start' }[$Matches[1]]
        try {
            Invoke-WebRequest "$base/api/v2/torrents/$new" -Method Post -WebSession $session -Headers $headers -UseBasicParsing -Body @{ hashes = $Matches[2] } | Out-Null
        } catch {
            Invoke-WebRequest "$base/api/v2/torrents/$($Matches[1])" -Method Post -WebSession $session -Headers $headers -UseBasicParsing -Body @{ hashes = $Matches[2] } | Out-Null
        }
        $message = if ($Matches[1] -eq 'pause') { 'Paused' } else { 'Resumed' }
    }

    $torrents = Invoke-RestMethod "$base/api/v2/torrents/info?sort=added_on&reverse=true" -WebSession $session -Headers $headers
} catch {
    $status = if ($_.Exception.Response) { [int]$_.Exception.Response.StatusCode } else { 0 }
    if ($status -eq 401 -or $status -eq 403) {
        Out-Result @(@{
            title = 'qBittorrent wants a login'
            subtitle = 'Set the Web UI password in Smowauncher Settings → Extensions (the username is in extension.toml)'
            icon = [string][char]0xE72E
            actions = @(@{ title = 'Open extension settings'; type = 'open'; value = (Join-Path $env:SMOW_EXTENSION_DIR 'extension.toml') })
        })
    }
    Out-Result @(@{
        title = "Can't reach qBittorrent at $base"
        subtitle = 'Is it running with the Web UI enabled? ' + $_.Exception.Message
        icon = [string][char]0xE783
        actions = @(@{ title = 'Open extension settings'; type = 'open'; value = (Join-Path $env:SMOW_EXTENSION_DIR 'extension.toml') })
    })
}

$paused = 'pausedDL', 'pausedUP', 'stoppedDL', 'stoppedUP'
$items = foreach ($t in $torrents) {
    if ($query -and $t.name -notlike "*$query*") { continue }
    $isPaused = $paused -contains $t.state
    $parts = @('{0:0.#}%' -f ($t.progress * 100))
    if ($t.progress -lt 1) {
        if ($t.dlspeed -gt 0) { $parts += "↓ $(Format-Size $t.dlspeed)/s" }
        $eta = Format-Eta $t.eta
        if ($eta -and -not $isPaused) { $parts += "ETA $eta" }
    } elseif ($t.upspeed -gt 0) {
        $parts += "↑ $(Format-Size $t.upspeed)/s"
    }
    $parts += Format-Size $t.size
    $toggle = if ($isPaused) { @{ title = 'Resume'; type = 'run'; value = "resume:$($t.hash)" } } else { @{ title = 'Pause'; type = 'run'; value = "pause:$($t.hash)" } }
    @{
        title = $t.name
        subtitle = $parts -join ' · '
        badge = $t.state
        icon = if ($t.progress -ge 1) { [string][char]0xE73E } else { [string][char]0xE896 }
        progress = [double]$t.progress
        actions = @(
            @{ title = 'Open Web UI'; type = 'open'; value = $base },
            $toggle,
            @{ title = 'Copy magnet link'; type = 'copy'; value = $t.magnet_uri }
        )
    }
}
if (-not $items) {
    $items = @(@{ title = if ($query) { "No torrents match `"$query`"" } else { 'No torrents' }; icon = [string][char]0xE896 })
}
Out-Result $items $message
