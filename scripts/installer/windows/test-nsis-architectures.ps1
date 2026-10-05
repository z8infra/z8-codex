$ErrorActionPreference = 'Stop'
$repoRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../../..'))
$makensis = "${env:ProgramFiles(x86)}\NSIS\makensis.exe"
if (-not (Test-Path -LiteralPath $makensis)) {
    throw "NSIS is required for this test: $makensis"
}

$tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$fixtureRoot = Join-Path $tempRoot ("z8-nsis-arch-" + [guid]::NewGuid().ToString('N'))
$scriptDirectory = Join-Path $fixtureRoot 'scripts/installer/windows'
$appDirectory = Join-Path $fixtureRoot 'dist/windows/app'
$iconDirectory = Join-Path $fixtureRoot 'apps/codex-plus-manager/src-tauri/icons'
New-Item -ItemType Directory -Path $scriptDirectory, $appDirectory, $iconDirectory -Force | Out-Null

try {
    Copy-Item -LiteralPath (Join-Path $repoRoot 'scripts/installer/windows/CodexPlusPlus.nsi') -Destination $scriptDirectory
    Copy-Item -LiteralPath (Join-Path $repoRoot 'apps/codex-plus-manager/src-tauri/icons/icon.ico') -Destination $iconDirectory
    Copy-Item -LiteralPath (Join-Path $repoRoot 'LICENSE') -Destination $fixtureRoot
    foreach ($name in @('z8-codex.exe', 'z8-codex-manager.exe')) {
        [System.IO.File]::WriteAllText(
            (Join-Path $appDirectory $name),
            'NSIS compile fixture only; never install or publish.'
        )
    }

    Push-Location $scriptDirectory
    try {
        foreach ($arch in @('x64', 'arm64')) {
            & $makensis /V1 /INPUTCHARSET UTF8 /DVERSION=1.3.8 "/DARCH=$arch" CodexPlusPlus.nsi
            if ($LASTEXITCODE -ne 0) {
                throw "NSIS $arch compile failed with exit code $LASTEXITCODE"
            }
            $package = Join-Path $fixtureRoot "dist/windows/Z8Codex-1.3.8-windows-$arch-setup.exe"
            if (-not (Test-Path -LiteralPath $package)) {
                throw "NSIS did not create $package"
            }
        }
        $previousErrorActionPreference = $ErrorActionPreference
        try {
            # NSIS writes the expected validation error to stderr. Temporarily
            # allow that native stderr so the exit code can be asserted below.
            $ErrorActionPreference = 'Continue'
            & $makensis /V1 /INPUTCHARSET UTF8 /DVERSION=1.3.8 /DARCH=invalid CodexPlusPlus.nsi 2>&1 | Out-Null
            $invalidArchitectureExitCode = $LASTEXITCODE
        }
        finally {
            $ErrorActionPreference = $previousErrorActionPreference
        }
        if ($invalidArchitectureExitCode -eq 0) {
            throw 'NSIS must reject an unknown architecture'
        }
    }
    finally {
        Pop-Location
    }
    Write-Output 'NSIS architecture compile fixtures: x64, arm64, invalid all passed'
}
finally {
    $resolvedFixture = [System.IO.Path]::GetFullPath($fixtureRoot)
    if (-not $resolvedFixture.StartsWith($tempRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
        $resolvedFixture -eq $tempRoot) {
        throw "Refusing to remove fixture outside temp directory: $resolvedFixture"
    }
    Remove-Item -LiteralPath $resolvedFixture -Recurse -Force
}
