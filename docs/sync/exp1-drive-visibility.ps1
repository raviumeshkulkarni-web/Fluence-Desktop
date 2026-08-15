# Experiment 1 harness - cross-client drive.file visibility in one GCP project.
# Runbook: docs/sync/experiments.md. Paste all output back to the orchestrator.
#
# Tests whether two clients (Desktop + Android) with only the drive.file scope can
# both see files created by the other in the app folder ("Fluence Transcribe").
#
# Usage:
#   A) Get a token first (exp4-pkce.ps1), then:
#      .\exp1-drive-visibility.ps1 -AccessToken $env:EXP_ACCESS_TOKEN -Account <throwaway>
#   B) Or let this script run the PKCE flow itself:
#      .\exp1-drive-visibility.ps1 -ClientId <desktop-client-id> -Account <throwaway>
#
# The Android side runs on the device: create <UUID>.json in the same folder with
# the same account. The harness then lists and reports visibility both ways.

param(
    [string]$AccessToken,
    [string]$ClientId,
    [string]$ClientSecret,
    [int]$Port = 58611,
    [Parameter(Mandatory = $true)]
    [string]$Account,
    [string]$FolderName = 'Fluence Transcribe'
)

$ErrorActionPreference = 'Stop'

$apiBase      = 'https://www.googleapis.com/drive/v3'
$UUID_A       = '00000000-0000-4000-8000-00000000000A'
$UUID_B       = '00000000-0000-4000-8000-00000000000B'

if (-not $AccessToken) {
    if (-not $ClientId) { throw 'Provide -AccessToken or -ClientId (to run exp4 PKCE first).' }
    if (-not $ClientSecret) {
        $ClientSecret = Read-Host 'Enter the OAuth client secret'
    }
    Write-Host "No token given - running loopback PKCE (exp4) first."
    $exp4 = Join-Path $PSScriptRoot 'exp4-pkce.ps1'
    $exp4Args = @{
        ClientId = $ClientId
        Port     = $Port
        Account  = $Account
    }
    if ($ClientSecret) { $exp4Args['ClientSecret'] = $ClientSecret }
    $out = & $exp4 @exp4Args
    $out | Out-Host
    $match = $out | Select-String -SimpleMatch 'Access token (memory only, for exp1 harness): '
    if (-not $match) { throw 'Could not extract access token from exp4 output.' }
    $matchLines = @($match | ForEach-Object { $_.Line })
    if ($matchLines.Count -gt 1) { throw "Multiple token lines matched ($($matchLines.Count)) - ambiguous extraction." }
    $AccessToken = ($matchLines[0] -split 'Access token \(memory only, for exp1 harness\): ')[1].Trim()
    $tokenFile = Join-Path $env:TEMP 'flu-exp1-access-token.txt'
    if (Test-Path -LiteralPath $tokenFile) {
        $AccessToken = (Get-Content -LiteralPath $tokenFile -Raw).Trim()
        Write-Host "Read access token byte-exact from temp file (length=$($AccessToken.Length))" -ForegroundColor Cyan
    }
    if (-not $AccessToken) { throw 'Empty access token from exp4 output.' }
    $ti = Invoke-RestMethod -Method Get -Uri "https://oauth2.googleapis.com/tokeninfo?access_token=$AccessToken"
    Write-Host "Token validated by Google tokeninfo: aud=$($ti.aud) scope='$($ti.scope)' expires_in=$($ti.expires_in)s email=$($ti.email)" -ForegroundColor Cyan
}

function Invoke-GDrive([string]$Method, [string]$Url, $Body = $null) {
    $params = @{
        Method  = $Method
        Uri     = $Url
        Headers = @{ Authorization = "Bearer $AccessToken" }
    }
    if ($null -ne $Body) {
        $params.ContentType = 'application/json'
        $params.Body = ($Body | ConvertTo-Json -Depth 10)
    }
    try {
        return Invoke-RestMethod @params
    } catch {
        $status = $_.Exception.Response.StatusCode.value__
        $msg = $_.ErrorDetails.Message
        Write-Host "DRIVE CALL FAILED HTTP $status : $msg" -ForegroundColor Red
        throw
    }
}

function Invoke-GDriveUpload([string]$Url, [hashtable]$Metadata, [string]$ParentFolderId, $Content) {
    $mediaUri = 'https://www.googleapis.com/upload/drive/v3/files?uploadType=media&fields=id'
    $media = Invoke-GDrive POST $mediaUri $Content
    $patchBody = @{ name = $Metadata.name }
    $add = [uri]::EscapeDataString($ParentFolderId)
    return Invoke-GDrive PATCH "$apiBase/files/$($media.id)?addParents=$add&fields=id,name,parents" $patchBody
}

function Get-OrCreate-Folder {
    $q = [uri]::EscapeDataString("name = '$FolderName' and mimeType = 'application/vnd.google-apps.folder' and trashed = false")
    $listing = Invoke-GDrive GET "$apiBase/files?q=$q&fields=files(id,name,mimeType)"
    if ($listing.files -and $listing.files.Count -gt 0) {
        return $listing.files[0]
    }
    $created = Invoke-GDrive POST "$apiBase/files?fields=id,name" @{
        name     = $FolderName
        mimeType = 'application/vnd.google-apps.folder'
    }
    return $created
}

function List-Children([string]$FolderId) {
    $q = [uri]::EscapeDataString("'$FolderId' in parents and trashed = false")
    $listing = Invoke-GDrive GET "$apiBase/files?q=$q&fields=files(id,name,parents,mimeType)"
    return $listing.files
}

function New-Record([string]$uuid, [string]$text) {
    $now = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    return @{
        v          = 1
        id         = $uuid
        created_at = $now
        deleted_at = $null
        text       = $text
        mode       = 'transcription'
        duration_ms = 1000
        provider   = 'groq'
        model      = 'whisper-large-v3'
        language   = 'en'
    }
}

Write-Host ""
Write-Host "== Experiment 1 - cross-client drive.file visibility =="
Write-Host "Date:      $(Get-Date -Format o)"
Write-Host "Account:   $Account"
Write-Host "Folder:    '$FolderName'"
Write-Host ""

$folder = Get-OrCreate-Folder
Write-Host "Folder id: $($folder.id)  name: $($folder.name)  mime: $($folder.mimeType)"
Write-Host ""

# Step 3: Windows client lists the sync folder. Is 000A (created by Android) present?
$before = List-Children $folder.id
Write-Host "-- Step 3: Windows lists folder --"
$visibleA = @($before | Where-Object { $_.name -eq "$UUID_A.json" })
if ($visibleA.Count -gt 0) {
    $f = $visibleA[0]
    Write-Host "PASS: $UUID_A.json IS visible. id=$($f.id) name=$($f.name) parents=$($f.parents -join ',')"
} else {
    Write-Host "FAIL: $UUID_A.json NOT visible. Listing contains: $((@($before | ForEach-Object name) -join ', '))"
}

# Step 4: Windows creates 000B. Android lists (done on device - print instructions).
$recordB = New-Record $UUID_B "Experiment 1 visibility probe from Windows (desktop client)."
$metadataB = @{ name = "$UUID_B.json"; mimeType = 'application/json'; parents = @($folder.id) }
$created = Invoke-GDriveUpload "$apiBase/files?uploadType=multipart&fields=id,name,parents" $metadataB $folder.id $recordB

Write-Host ""
Write-Host "-- Step 4: Windows created $UUID_B.json --"
Write-Host "id=$($created.id) name=$($created.name) parents=$($created.parents -join ',')"
Write-Host ""

# Re-list to confirm 000B visible to Windows too, then summarize for the Android side.
$after = List-Children $folder.id
$visibleB = @($after | Where-Object { $_.name -eq "$UUID_B.json" })
Write-Host "-- Windows re-list after create --"
Write-Host "Files now: $((@($after | ForEach-Object name) -join ', '))"
Write-Host ""

Write-Host "-- Step 4b: run on the Android device now --"
Write-Host " 1. On Android (same account, same project), list the '$FolderName' folder."
Write-Host " 2. Record whether $UUID_B.json is visible."
Write-Host " 3. Create $UUID_A.json there, then re-run this script to confirm step 3 flips to PASS."
Write-Host ""

# Step 5: cross-account probe - rerun with the OTHER account signed in on Android only.
Write-Host "-- Step 5: cross-account (record on device) --"
Write-Host " With a second account signed in on Android, list the folder."
Write-Host " Expected: NO files from account A are visible (namespace is per-account, spec 13)."
Write-Host ""

if ($visibleA.Count -gt 0) {
    Write-Host "VERDICT (so far): PASS - files created by the Android client are visible to the Desktop client listing with drive.file only."
} else {
    Write-Host "VERDICT (so far): FAIL - $UUID_A.json not visible. If it was created on Android and still absent, record the pivot (AppData + delegation)."
}
Write-Host ""
