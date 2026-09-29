param(
    [Parameter(Mandatory = $true)]
    [string] $Path,

    [Parameter(Mandatory = $true)]
    [ValidateSet('x64', 'arm64')]
    [string] $Arch
)

$ErrorActionPreference = 'Stop'
$expectedMachine = if ($Arch -eq 'arm64') { 0xAA64 } else { 0x8664 }
$resolvedPath = (Resolve-Path -LiteralPath $Path -ErrorAction Stop).ProviderPath
$stream = [System.IO.File]::OpenRead($resolvedPath)
try {
    $reader = [System.IO.BinaryReader]::new($stream)
    if ($stream.Length -lt 0x40 -or $reader.ReadUInt16() -ne 0x5A4D) {
        throw "Not a PE executable: $resolvedPath"
    }
    $stream.Position = 0x3C
    $peOffset = $reader.ReadUInt32()
    if ($peOffset -gt $stream.Length - 6) {
        throw "Invalid PE header offset: $resolvedPath"
    }
    $stream.Position = $peOffset
    if ($reader.ReadUInt32() -ne 0x00004550) {
        throw "Missing PE signature: $resolvedPath"
    }
    $actualMachine = $reader.ReadUInt16()
    if ($actualMachine -ne $expectedMachine) {
        throw ("PE machine mismatch for {0}: expected 0x{1:X4}, got 0x{2:X4}" -f $resolvedPath, $expectedMachine, $actualMachine)
    }
    Write-Output ("Verified {0} PE machine 0x{1:X4}: {2}" -f $Arch, $actualMachine, $resolvedPath)
}
finally {
    $stream.Dispose()
}
