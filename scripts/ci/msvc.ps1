# Configure the installed Visual Studio toolchain without a JavaScript action.
$ErrorActionPreference = 'Stop'
$architecture = switch ($env:RUNNER_ARCH) {
    'X64' { 'amd64' }
    'ARM64' { 'arm64' }
    default { throw "Unsupported Windows runner architecture: $env:RUNNER_ARCH" }
}
if ([string]::IsNullOrEmpty($env:GITHUB_ENV)) { throw 'GITHUB_ENV is required' }

$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio/Installer/vswhere.exe'
$installation = & $vswhere -latest -prerelease -products '*' -property installationPath
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($installation)) {
    throw 'Cannot locate the installed Visual Studio toolchain'
}
$launcher = Join-Path $installation.Trim() 'Common7/Tools/Launch-VsDevShell.ps1'
if (!(Test-Path -LiteralPath $launcher)) { throw "Missing Visual Studio developer shell: $launcher" }

$before = [Collections.Generic.Dictionary[string, string]]::new([StringComparer]::OrdinalIgnoreCase)
Get-ChildItem Env: | ForEach-Object { $before[$_.Name] = $_.Value }
& $launcher -Arch $architecture -HostArch $architecture -SkipAutomaticLocation

$expectedTarget = if ($architecture -eq 'amd64') { 'x64' } else { 'arm64' }
if ($env:VSCMD_ARG_TGT_ARCH -ne $expectedTarget -or
    [string]::IsNullOrWhiteSpace($env:VCToolsVersion) -or
    [string]::IsNullOrWhiteSpace($env:WindowsSDKVersion)) {
    throw 'Visual Studio did not configure the requested native compiler and SDK'
}
$compiler = Get-Command cl.exe -CommandType Application -ErrorAction Stop
$lines = [Collections.Generic.List[string]]::new()
foreach ($variable in Get-ChildItem Env:) {
    if (!$before.ContainsKey($variable.Name) -or $before[$variable.Name] -cne $variable.Value) {
        $delimiter = 'MSVC_' + [Guid]::NewGuid().ToString('N')
        $lines.Add("$($variable.Name)<<$delimiter")
        $lines.Add($variable.Value)
        $lines.Add($delimiter)
        Write-Host "Exporting $($variable.Name)"
    }
}
[IO.File]::AppendAllLines($env:GITHUB_ENV, $lines, [Text.UTF8Encoding]::new($false))
Write-Host "Native $expectedTarget compiler: $($compiler.Source)"
