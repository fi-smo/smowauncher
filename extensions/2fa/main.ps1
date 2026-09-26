# Smowauncher extension: TOTP codes. Secrets are stored DPAPI-encrypted (current Windows user)
# in $env:SMOW_DATA_DIR\codes.dat. Prints {"items": [...]} as JSON.
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.Encoding]::UTF8

$store = Join-Path $env:SMOW_DATA_DIR 'codes.dat'
$query = $env:SMOW_QUERY
$action = $env:SMOW_ACTION

function Out-Result($items, $message = '', $newQuery = $null) {
    $result = [ordered]@{ items = @($items); message = $message }
    if ($null -ne $newQuery) { $result.query = $newQuery }
    [Console]::Out.Write((ConvertTo-Json -InputObject ([pscustomobject]$result) -Depth 6 -Compress))
    exit 0
}

function Protect([string]$text) { ConvertFrom-SecureString (ConvertTo-SecureString $text -AsPlainText -Force) }
function Unprotect([string]$cipher) {
    $secure = ConvertTo-SecureString $cipher
    $ptr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure)
    try { [Runtime.InteropServices.Marshal]::PtrToStringBSTR($ptr) } finally { [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($ptr) }
}

function ConvertFrom-Base32([string]$s) {
    $alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ234567'
    $s = $s.ToUpper().TrimEnd('=').Replace(' ', '')
    $bits = 0; $value = 0
    $out = New-Object System.Collections.Generic.List[byte]
    foreach ($c in $s.ToCharArray()) {
        $i = $alphabet.IndexOf($c)
        if ($i -lt 0) { throw "bad base32" }
        # Only the low bits are ever needed; masking keeps the int from overflowing.
        $value = (($value -shl 5) -bor $i) -band 0xFFFF; $bits += 5
        if ($bits -ge 8) { $bits -= 8; $out.Add([byte](($value -shr $bits) -band 0xFF)) }
    }
    , $out.ToArray()
}

function Get-Totp([byte[]]$key, [int]$digits, [int]$period, [string]$algorithm) {
    $counter = [long][Math]::Floor([DateTimeOffset]::UtcNow.ToUnixTimeSeconds() / $period)
    $msg = [BitConverter]::GetBytes($counter)
    [Array]::Reverse($msg)
    $hmac = switch ($algorithm) {
        'SHA256' { New-Object System.Security.Cryptography.HMACSHA256 (, $key) }
        'SHA512' { New-Object System.Security.Cryptography.HMACSHA512 (, $key) }
        default { New-Object System.Security.Cryptography.HMACSHA1 (, $key) }
    }
    $h = $hmac.ComputeHash($msg)
    $o = $h[$h.Length - 1] -band 0x0F
    # Widen to int first: -shl on a [byte] stays a byte in Windows PowerShell (200 -shl 16 = 0).
    $bin = (([int]$h[$o] -band 0x7F) -shl 24) -bor ([int]$h[$o + 1] -shl 16) -bor ([int]$h[$o + 2] -shl 8) -bor [int]$h[$o + 3]
    ($bin % [Math]::Pow(10, $digits)).ToString().PadLeft($digits, '0')
}

function Parse-Uri([string]$uri) {
    if ($uri -notmatch '^otpauth://totp/([^?]*)\?(.*)$') { return $null }
    $label = [Uri]::UnescapeDataString($Matches[1])
    $params = @{}
    foreach ($p in $Matches[2].Split('&')) {
        $kv = $p.Split('=', 2)
        if ($kv.Count -eq 2) { $params[$kv[0].ToLower()] = [Uri]::UnescapeDataString($kv[1]) }
    }
    if (-not $params.secret) { return $null }
    $issuer = $params.issuer; $account = $label
    if ($label -match '^([^:]+):\s*(.*)$') { if (-not $issuer) { $issuer = $Matches[1] }; $account = $Matches[2] }
    [pscustomobject]@{
        issuer = if ($issuer) { $issuer } else { $account }
        account = $account
        secret = $params.secret
        digits = if ($params.digits) { [int]$params.digits } else { 6 }
        period = if ($params.period) { [int]$params.period } else { 30 }
        algorithm = if ($params.algorithm) { $params.algorithm.ToUpper() } else { 'SHA1' }
    }
}

# --- import / delete-export actions --------------------------------------------------------
if ($action -match '^import:(.+)$') {
    $path = $Matches[1]
    $uris = @(Get-Content -LiteralPath $path -Encoding UTF8 | Where-Object { $_ -match '^otpauth://totp/' })
    if (-not $uris) { Out-Result @(@{ title = "No otpauth://totp links found in $path"; icon = [string][char]0xE783 }) }
    Set-Content -LiteralPath $store -Value (Protect ($uris -join "`n")) -Encoding ASCII
    Out-Result @(@{
        title = "Imported $($uris.Count) codes — now delete the plain-text export"
        subtitle = $path
        icon = [string][char]0xE72E
        actions = @(@{ title = 'Delete the export file'; type = 'run'; value = "delete-export:$path" })
    }) "Imported $($uris.Count) codes"
}
if ($action -match '^delete-export:(.+)$') {
    Remove-Item -LiteralPath $Matches[1] -Force
    Out-Result @() 'Export file deleted' ''
}
if ($query -match '^import\s+"?(.+?)"?$') {
    $path = $Matches[1]
    $ok = Test-Path -LiteralPath $path -PathType Leaf
    Out-Result @(@{
        title = if ($ok) { "Import 2FA codes from $([IO.Path]::GetFileName($path))" } else { 'File not found' }
        subtitle = if ($ok) { 'Encrypted for your Windows account; replaces previously imported codes' } else { $path }
        icon = [string][char]0xE8B5
        actions = @(if ($ok) { @{ title = 'Import'; type = 'run'; value = "import:$path" } })
    })
}

# --- codes ---------------------------------------------------------------------------------
if (-not (Test-Path -LiteralPath $store)) {
    Out-Result @(@{
        title = 'No codes yet: type "2fa import <path to export file>"'
        subtitle = 'Ente Auth → Settings → Data → Export codes → Plain text'
        icon = [string][char]0xE8B5
    })
}
$entries = foreach ($line in (Unprotect (Get-Content -LiteralPath $store -Raw).Trim()).Split("`n")) { Parse-Uri $line.Trim() }
$now = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
$items = foreach ($e in $entries) {
    if (-not $e) { continue }
    if ($query -and "$($e.issuer) $($e.account)" -notlike "*$query*") { continue }
    try { $code = Get-Totp (ConvertFrom-Base32 $e.secret) $e.digits $e.period $e.algorithm } catch { continue }
    $left = $e.period - ($now % $e.period)
    $shown = if ($code.Length -eq 6) { $code.Substring(0, 3) + ' ' + $code.Substring(3) } else { $code }
    @{
        title = $e.issuer
        subtitle = "$($e.account) · $left s"
        badge = $shown
        icon = [string][char]0xE8D7
        progress = [double]($left / $e.period)
        actions = @(
            @{ title = 'Copy code'; type = 'copy'; value = $code },
            @{ title = 'Paste code'; type = 'paste'; value = $code }
        )
    }
}
if (-not $items) { $items = @(@{ title = "No codes match `"$query`""; icon = [string][char]0xE8D7 }) }
Out-Result $items
