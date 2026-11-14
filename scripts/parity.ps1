# Byte parity check against the frozen reference outputs under D:\Downloads\Brave\159.
#
# For every set:
#   1. split <set>.edz into .\parity\<set>
#   2. compare the output file names with the reference directory
#   3. compare SHA-256 of every shared file
#
# Writes only under the project tree; D: is read-only for this unsigned binary.
param(
    [int]$Jobs = 0
)

$ErrorActionPreference = 'Stop'
$projectRoot = Split-Path -Parent $PSScriptRoot
$binary = Join-Path $projectRoot 'target\release\edzsplit.exe'
if (-not (Test-Path $binary)) {
    $binary = Join-Path $projectRoot 'target\debug\edzsplit.exe'
}
if (-not (Test-Path $binary)) { throw "binary not found: $binary" }
$srcRoot = 'D:\Downloads\Brave\159'
$outRoot = Join-Path $projectRoot 'parity'
$reportRoot = Join-Path $outRoot '_reports'
New-Item -ItemType Directory -Force $reportRoot | Out-Null

$fileMap = [ordered]@{
    'Hitachi-Industrial-Equipment' = 'HTC'
    'HMS'                         = 'HMS'
    'IDS'                         = 'IDS'
    'Jetter'                      = 'Jetter'
    'Meltric'                     = 'Meltric'
    'Secomea'                     = 'Secomea'
    'SES-Sterling'                = 'SES-Sterling'
    'SEW-EURODRIVE_1'             = 'SEW-EURODRIVE_1'
    'SIKA'                        = 'SIKA'
    'Wittenstein'                 = 'Wittenstein'
}
$rows = @()
foreach ($set in $fileMap.Keys) {
    $src = Join-Path $srcRoot "$set.edz"
    $ref = Join-Path $srcRoot $fileMap[$set]
    if (-not (Test-Path $src)) {
        $rows += [pscustomobject]@{ Set=$set; ExitCode=99; RefCount=0; NewCount=0; NamesOK=$false; Identical=0; Differing='SOURCE MISSING'; Seconds=0 }
        continue
    }
    if (-not (Test-Path $ref)) {
        $rows += [pscustomobject]@{ Set=$set; ExitCode=99; RefCount=0; NewCount=0; NamesOK=$false; Identical=0; Differing='REFERENCE MISSING'; Seconds=0 }
        continue
    }

    $outDir = Join-Path $outRoot $set
    Remove-Item $outDir -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $outDir | Out-Null

    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    if ($Jobs -gt 0) {
        & $binary split $src --out $outDir --jobs $Jobs *> (Join-Path $reportRoot "$set.jsonl")
    } else {
        & $binary split $src --out $outDir *> (Join-Path $reportRoot "$set.jsonl")
    }
    $exitCode = $LASTEXITCODE
    $sw.Stop()

    $refFiles = @(Get-ChildItem $ref -Filter *.edz | Sort-Object Name)
    $newFiles = @(Get-ChildItem $outDir -Filter *.edz | Sort-Object Name)
    $refNames = $refFiles | ForEach-Object { $_.Name }
    $newNames = $newFiles | ForEach-Object { $_.Name }
    $namesEqual = (($refNames -join '|') -ceq ($newNames -join '|'))

    $identical = 0
    $differing = New-Object System.Collections.Generic.List[string]
    foreach ($f in $refFiles) {
        $n = Join-Path $outDir $f.Name
        if (-not (Test-Path $n)) { $differing.Add("MISSING $($f.Name)"); continue }
        $a = (Get-FileHash $f.FullName -Algorithm SHA256).Hash
        $b = (Get-FileHash $n -Algorithm SHA256).Hash
        if ($a -ceq $b) { $identical++ } else { $differing.Add($f.Name) }
    }

$rows += [pscustomobject]@{
        Set       = $set
        ExitCode  = $exitCode
        RefCount  = $refFiles.Count
        NewCount  = $newFiles.Count
        NamesOK   = $namesEqual
        Identical = $identical
        Differing = ($differing -join ';')
        Seconds   = [math]::Round($sw.Elapsed.TotalSeconds, 1)
    }
    Write-Host ("{0,-22} exit={1} ref={2} new={3} identical={4} time={5}s" -f $set, $exitCode, $refFiles.Count, $newFiles.Count, $identical, [math]::Round($sw.Elapsed.TotalSeconds, 1))
}

$rows | Format-Table -AutoSize
$rows | Export-Csv (Join-Path $reportRoot 'parity.csv') -NoTypeInformation -Encoding UTF8
$bad = $rows | Where-Object { $_.ExitCode -ge 4 -or $_.NamesOK -ne $true -or ($_.Differing -ne '') }
if ($bad) {
    Write-Host "`nFAILED SETS:"
    $bad | Format-Table -AutoSize
    exit 3
}
Write-Host "`nPARITY PASS"
