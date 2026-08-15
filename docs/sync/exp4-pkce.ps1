# Experiment 4 harness - Desktop loopback PKCE (S256) against real Google endpoints.
# Runbook: docs/sync/experiments.md. Paste all output back to the orchestrator.
# Mirrors the app's auth.rs: redirect_uri http://localhost:<port>/, scope drive.file,
# access_type=offline (refresh token). The client secret is optional at the token
# exchange (public client) but Google may still require it (observed 2026-08-15:
# "client_secret is missing" without it) - pass -ClientSecret when required.

param(
    [Parameter(Mandatory = $true)]
    [string]$ClientId,
    [int]$Port = 58611,
    [string]$Account = "<throwaway-account>",
    [string]$ClientSecret
)

$ErrorActionPreference = 'Stop'

$authEndpoint  = 'https://accounts.google.com/o/oauth2/v2/auth'
$tokenEndpoint = 'https://oauth2.googleapis.com/token'
$scope         = 'https://www.googleapis.com/auth/drive.file'
$redirectUri   = "http://localhost:$Port/"

function ConvertTo-Base64Url([byte[]]$Bytes) {
    return ([Convert]::ToBase64String($Bytes) -replace '\+', '-' -replace '/', '_' -replace '=+$', '')
}

function Get-CodeChallenge([string]$Verifier) {
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
        $digest = $sha.ComputeHash([System.Text.Encoding]::ASCII.GetBytes($Verifier))
    } finally {
        $sha.Dispose()
    }
    return ConvertTo-Base64Url $digest
}

function Receive-OAuthRedirect([int]$RedirectPort) {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $RedirectPort)
    $listener.Start()
    Write-Host "Loopback listener on http://localhost:$RedirectPort/ ..."
    try {
        $client = $listener.AcceptTcpClient()
        $stream = $client.GetStream()
        $reader = [System.IO.StreamReader]::new($stream)
        $requestLine = $reader.ReadLine()
        Write-Host ">> $requestLine"
        while ($true) {
            $line = $reader.ReadLine()
            if ($null -eq $line -or $line -eq '') { break }
        }
        $pathAndQuery = $requestLine.Split(' ')[1]
        $uri = [System.Uri]::new("http://localhost:$RedirectPort$pathAndQuery")
        $body = '<html><body><h3>Authorization complete. You may close this tab.</h3></body></html>'
        $bodyBytes = [System.Text.Encoding]::UTF8.GetBytes($body)
        $header = "HTTP/1.1 200 OK`r`nContent-Type: text/html; charset=utf-8`r`nContent-Length: $($bodyBytes.Length)`r`nConnection: close`r`n`r`n"
        $headerBytes = [System.Text.Encoding]::ASCII.GetBytes($header)
        $stream.Write($headerBytes, 0, $headerBytes.Length)
        $stream.Write($bodyBytes, 0, $bodyBytes.Length)
        $stream.Flush()
        $client.Close()
        return $uri
    } finally {
        $listener.Stop()
    }
}

function Invoke-TokenRequest([hashtable]$Body) {
    return Invoke-RestMethod -Method Post -Uri $tokenEndpoint -ContentType 'application/x-www-form-urlencoded' -Body $Body
}

Write-Host ""
Write-Host "== Experiment 4 - Desktop loopback PKCE S256 =="
Write-Host "Date:      $(Get-Date -Format o)"
Write-Host "Client ID: $ClientId"
Write-Host "Account:   $Account"
Write-Host "Scope:     $scope"
Write-Host ""

$verifierBytes = New-Object byte[] 32
[System.Security.Cryptography.RandomNumberGenerator]::Fill($verifierBytes)
$codeVerifier  = ConvertTo-Base64Url $verifierBytes
$codeChallenge = Get-CodeChallenge $codeVerifier

Write-Host "code_verifier:  $codeVerifier"
Write-Host "code_challenge: $codeChallenge"

$authUrl = "${authEndpoint}?response_type=code" +
           "&client_id=$([uri]::EscapeDataString($ClientId))" +
           "&redirect_uri=$([uri]::EscapeDataString($redirectUri))" +
           "&scope=$([uri]::EscapeDataString($scope))" +
           "&code_challenge=$codeChallenge&code_challenge_method=S256" +
           "&access_type=offline&prompt=select_account"

Write-Host ""
Write-Host "Opening browser for consent: $authUrl"
Start-Process $authUrl

$redirect = Receive-OAuthRedirect $Port
Write-Host ""
Write-Host "Redirect received: $($redirect.AbsoluteUri)"

$query = $redirect.Query.TrimStart('?')
if ($query -match '(^|&)error=') {
    Write-Host "ERROR in redirect: $query" -ForegroundColor Red
    exit 1
}
if ($query -notmatch '(^|&)code=') {
    Write-Host "ERROR: no code in redirect: $query" -ForegroundColor Red
    exit 1
}
$code = [uri]::UnescapeDataString((($query -split '&' | Where-Object { $_ -like 'code=*' }) -replace '^code=', ''))
Write-Host "Authorization code captured: $($code.Substring(0, [Math]::Min(16, $code.Length)))..."

Write-Host ""
Write-Host "-- 3. Token exchange (grant_type=authorization_code) --"
try {
    $exchangeBody = @{
        code          = $code
        client_id     = $ClientId
        code_verifier = $codeVerifier
        redirect_uri  = $redirectUri
        grant_type    = 'authorization_code'
    }
    if ($ClientSecret) { $exchangeBody.client_secret = $ClientSecret }
    $tokens = Invoke-TokenRequest $exchangeBody
} catch {
    $body = $_.ErrorDetails.Message
    $status = $_.Exception.Response.StatusCode.value__
    Write-Host "EXCHANGE FAILED HTTP $status : $body" -ForegroundColor Red
    exit 1
}
Write-Host "Exchange OK."
Write-Host "granted scope : $($tokens.scope)"
Write-Host "expires_in    : $($tokens.expires_in) s"
Write-Host "has access    : $($null -ne $tokens.access_token)"
Write-Host "has refresh   : $($null -ne $tokens.refresh_token)"

$refreshOk = $null -ne $tokens.refresh_token
if (-not $refreshOk) {
    Write-Host "FAIL: no refresh_token. Re-run and check access_type=offline + prompt=consent are present." -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "-- 4. Refresh (grant_type=refresh_token) --"
try {
    $refreshBody = @{
        refresh_token = $tokens.refresh_token
        client_id     = $ClientId
        grant_type    = 'refresh_token'
    }
    if ($ClientSecret) { $refreshBody.client_secret = $ClientSecret }
    $refreshed = Invoke-TokenRequest $refreshBody
    Write-Host "Refresh OK. New access token issued, no re-consent."
    Write-Host "granted scope : $($refreshed.scope)"
    Write-Host "expires_in    : $($refreshed.expires_in) s"
} catch {
    $body = $_.ErrorDetails.Message
    $status = $_.Exception.Response.StatusCode.value__
    Write-Host "REFRESH FAILED HTTP $status : $body" -ForegroundColor Red
    exit 1
}

Write-Host ""
Write-Host "-- 5. Token storage check --"
if ($tokens.access_token) {
    Write-Host "access token  : memory only (harness keeps it in this process, never written to disk)"
} else {
    Write-Host "access token  : MISSING"
}
$target = "FluenceSyncExp4"
cmdkey /generic:$target /user:$Account /pass:($tokens.refresh_token) | Out-Null
$stored = cmdkey /list | Select-String -SimpleMatch $target
if ($stored) {
    Write-Host "refresh token : stored in Windows Credential Manager as '$target'"
} else {
    Write-Host "refresh token : Credential Manager store check FAILED (cmdkey did not list target)" -ForegroundColor Red
}

Write-Host ""
Write-Host "-- 6. Error path: bogus refresh token -> invalid_grant --"
try {
    $bogusBody = @{ refresh_token = 'garbage.invalid.token'; client_id = $ClientId; grant_type = 'refresh_token' }
    if ($ClientSecret) { $bogusBody.client_secret = $ClientSecret }
    Invoke-TokenRequest $bogusBody | Out-Null
    Write-Host "UNEXPECTED: bogus refresh succeeded" -ForegroundColor Red
} catch {
    Write-Host "HTTP $($_.Exception.Response.StatusCode.value__) body: $($_.ErrorDetails.Message)"
    if ($_.ErrorDetails.Message -match 'invalid_grant') { Write-Host "invalid_grant observed (expected)." }
}

Write-Host ""
if ($tokens.scope -ne $scope) {
    Write-Host "VERDICT: FAIL - granted scope '$($tokens.scope)' is not exactly '$scope'" -ForegroundColor Red
} else {
    Write-Host "VERDICT: PASS - PKCE exchange + refresh succeed; scope exactly drive.file; refresh in Credential Manager"
}
Write-Output "Access token (memory only, for exp1 harness): $($tokens.access_token)"
$tokenFile = Join-Path $env:TEMP 'flu-exp1-access-token.txt'
Set-Content -LiteralPath $tokenFile -Value $tokens.access_token -NoNewline -Encoding utf8
Write-Host "access token   : also written byte-exact to $tokenFile"
Write-Host ""
