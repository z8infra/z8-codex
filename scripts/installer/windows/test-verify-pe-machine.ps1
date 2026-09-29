$ErrorActionPreference = 'Stop'
$tempRoot = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
$fixtureRoot = Join-Path $tempRoot ("z8-pe-machine-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $fixtureRoot | Out-Null

function Write-TestPe {
    param([string] $Path, [int] $Machine)
    $bytes = [byte[]]::new(256)
    $bytes[0] = 0x4D
    $bytes[1] = 0x5A
    $bytes[0x3C] = 0x80
    $bytes[0x80] = 0x50
    $bytes[0x81] = 0x45
    $bytes[0x84] = [byte]($Machine -band 0xFF)
    $bytes[0x85] = [byte](($Machine -shr 8) -band 0xFF)
    [System.IO.File]::WriteAllBytes($Path, $bytes)
}

try {
    $x64 = Join-Path $fixtureRoot 'x64.exe'
    $arm64 = Join-Path $fixtureRoot 'arm64.exe'
    $invalid = Join-Path $fixtureRoot 'invalid.exe'
    Write-TestPe -Path $x64 -Machine 0x8664
    Write-TestPe -Path $arm64 -Machine 0xAA64
    [System.IO.File]::WriteAllBytes($invalid, [byte[]]::new(16))

    & "$PSScriptRoot/verify-pe-machine.ps1" -Path $x64 -Arch x64 | Out-Null
    & "$PSScriptRoot/verify-pe-machine.ps1" -Path $arm64 -Arch arm64 | Out-Null

    foreach ($case in @(
        @{ Path = $x64; Arch = 'arm64'; Error = 'PE machine mismatch' },
        @{ Path = $arm64; Arch = 'x64'; Error = 'PE machine mismatch' },
        @{ Path = $invalid; Arch = 'x64'; Error = 'Not a PE executable' }
    )) {
        $failed = $false
        try {
            & "$PSScriptRoot/verify-pe-machine.ps1" -Path $case.Path -Arch $case.Arch | Out-Null
        }
        catch {
            $failed = $_.Exception.Message.Contains($case.Error)
        }
        if (-not $failed) {
            throw "Expected $($case.Error) for $($case.Path) as $($case.Arch)"
        }
    }
    Write-Output 'PE machine verifier fixtures: 5/5 passed'
}
finally {
    $resolvedFixture = [System.IO.Path]::GetFullPath($fixtureRoot)
    if (-not $resolvedFixture.StartsWith($tempRoot, [System.StringComparison]::OrdinalIgnoreCase) -or
        $resolvedFixture -eq $tempRoot) {
        throw "Refusing to remove fixture outside temp directory: $resolvedFixture"
    }
    Remove-Item -LiteralPath $resolvedFixture -Recurse -Force
}
