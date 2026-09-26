# Smowauncher extension: Jenkins REST API. Prints {"items": [...]} as JSON.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$base = $env:SMOW_SETTING_URL.TrimEnd('/')
$query = $env:SMOW_QUERY
$action = $env:SMOW_ACTION

function Out-Result($items, $message = '') {
    [Console]::Out.Write((ConvertTo-Json -InputObject ([pscustomobject]@{ items = @($items); message = $message }) -Depth 6 -Compress))
    exit 0
}

function Format-Ago([long]$ms) {
    $t = [DateTime]::UtcNow - [DateTimeOffset]::FromUnixTimeMilliseconds($ms).UtcDateTime
    if ($t.TotalMinutes -lt 1) { return 'just now' }
    if ($t.TotalHours -lt 1) { return '{0} min ago' -f [int]$t.TotalMinutes }
    if ($t.TotalDays -lt 1) { return '{0} h ago' -f [int]$t.TotalHours }
    return '{0} days ago' -f [int]$t.TotalDays
}

$headers = @{}
if ($env:SMOW_SECRET_TOKEN) {
    $pair = [Text.Encoding]::UTF8.GetBytes("$($env:SMOW_SETTING_USER):$($env:SMOW_SECRET_TOKEN)")
    $headers.Authorization = 'Basic ' + [Convert]::ToBase64String($pair)
}

$message = ''
try {
    if ($action -match '^build:(.+)$') {
        $job = $Matches[1]
        try {
            Invoke-WebRequest "$job/build" -Method Post -Headers $headers -UseBasicParsing | Out-Null
        } catch {
            # Parameterized jobs only accept buildWithParameters (default values are used).
            Invoke-WebRequest "$job/buildWithParameters" -Method Post -Headers $headers -UseBasicParsing | Out-Null
        }
        $message = 'Build started'
    }
    $tree = 'jobs[name,url,color,lastBuild[number,result,timestamp,building,url]]'
    $data = Invoke-RestMethod "$base/api/json?tree=$tree" -Headers $headers
} catch {
    $status = if ($_.Exception.Response) { [int]$_.Exception.Response.StatusCode } else { 0 }
    if ($status -eq 401 -or $status -eq 403) {
        Out-Result @(@{
            title = 'Jenkins wants a login'
            subtitle = 'Set your API token in Smowauncher Settings → Extensions (the user name is in extension.toml)'
            icon = [string][char]0xE72E
            actions = @(@{ title = 'Open extension settings'; type = 'open'; value = (Join-Path $env:SMOW_EXTENSION_DIR 'extension.toml') })
        })
    }
    Out-Result @(@{
        title = "Can't reach Jenkins at $base"
        subtitle = 'Check the URL and user in extension.toml and the token in Settings → Extensions. ' + $_.Exception.Message
        icon = [string][char]0xE783
        actions = @(@{ title = 'Open extension settings'; type = 'open'; value = (Join-Path $env:SMOW_EXTENSION_DIR 'extension.toml') })
    })
}

$items = foreach ($j in $data.jobs) {
    if ($query -and $j.name -notlike "*$query*") { continue }
    $b = $j.lastBuild
    if (-not $j.color) {
        # A folder: just open it.
        @{ title = $j.name; subtitle = 'Folder'; icon = [string][char]0xE8B7; actions = @(@{ title = 'Open in browser'; type = 'open'; value = $j.url }) }
        continue
    }
    $status = if ($b -and $b.building) { 'building…' } elseif ($b) { "$($b.result)".ToLower() } else { 'never built' }
    $subtitle = if ($b) { "#$($b.number) $status · $(Format-Ago $b.timestamp)" } else { $status }
    $actions = @(
        @{ title = 'Build now'; type = 'run'; value = "build:$($j.url.TrimEnd('/'))" },
        @{ title = 'Open in browser'; type = 'open'; value = $j.url }
    )
    if ($b) { $actions += @{ title = 'Open last build console'; type = 'open'; value = "$($b.url)console" } }
    @{
        title = $j.name
        subtitle = $subtitle
        badge = $status
        icon = if ($b -and $b.building) { [string][char]0xE895 } elseif ($b.result -eq 'SUCCESS') { [string][char]0xE73E } elseif ($b.result -eq 'FAILURE') { [string][char]0xE711 } else { [string][char]0xE9F5 }
        actions = $actions
    }
}
if (-not $items) {
    $items = @(@{ title = if ($query) { "No jobs match `"$query`"" } else { 'No jobs' }; icon = [string][char]0xE9F5 })
}
Out-Result $items $message
