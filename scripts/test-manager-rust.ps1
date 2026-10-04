param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]] $TestArguments
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$manifest = Join-Path $repoRoot 'apps/codex-plus-manager/src-tauri/windows-app-manifest.xml'
$probeDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ('z8-manager-test-' + [guid]::NewGuid().ToString('N'))

Push-Location $repoRoot
try {
    $cargoLines = & cargo test --locked -p codex-plus-manager --lib --no-run --message-format=json
    if ($LASTEXITCODE -ne 0) {
        throw "Tauri Manager test compilation failed: $LASTEXITCODE"
    }

    $testArtifact = $cargoLines |
        ForEach-Object {
            if ($_.StartsWith('{')) { $_ | ConvertFrom-Json }
        } |
        Where-Object {
            $_.reason -eq 'compiler-artifact' -and
            $_.target.name -eq 'codex_plus_manager_lib' -and
            $_.profile.test -and
            $_.executable
        } |
        Select-Object -Last 1
    if (-not $testArtifact) {
        throw 'Tauri Manager test executable was not reported by Cargo'
    }

    New-Item -ItemType Directory -Path $probeDirectory | Out-Null
    $testCopy = Join-Path $probeDirectory ([System.IO.Path]::GetFileName($testArtifact.executable))
    Copy-Item -LiteralPath $testArtifact.executable -Destination $testCopy
    Copy-Item -LiteralPath $manifest -Destination ($testCopy + '.manifest')
    if ((Get-FileHash -LiteralPath $testCopy -Algorithm SHA256).Hash -ne
        (Get-FileHash -LiteralPath $testArtifact.executable -Algorithm SHA256).Hash) {
        throw 'Copied Manager test executable differs from Cargo artifact'
    }

    $arguments = if ($TestArguments.Count -gt 0) { $TestArguments } else { @('--test-threads=1') }
    & $testCopy @arguments
    $testExit = $LASTEXITCODE
} finally {
    if (Test-Path -LiteralPath $probeDirectory) {
        Remove-Item -LiteralPath $probeDirectory -Force -Recurse
    }
    Pop-Location
}

exit $testExit
