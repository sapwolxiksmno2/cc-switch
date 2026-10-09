[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string]$ExecutablePath,

    [Parameter(Mandatory)]
    [string]$OutputDirectory,

    [string]$ProjectRoot = (Split-Path $PSScriptRoot -Parent)
)

$ErrorActionPreference = 'Stop'

if (-not [IO.File]::Exists($ExecutablePath)) {
    throw 'Portable 可执行文件不存在，或路径不是文件。'
}
if ([IO.FileInfo]::new($ExecutablePath).Length -eq 0) {
    throw 'Portable 可执行文件不能为空。'
}

try {
    $package = [IO.File]::ReadAllText((Join-Path $ProjectRoot 'package.json')) | ConvertFrom-Json
    $tauri = [IO.File]::ReadAllText((Join-Path $ProjectRoot 'src-tauri/tauri.conf.json')) | ConvertFrom-Json
    $cargo = [IO.File]::ReadAllText((Join-Path $ProjectRoot 'src-tauri/Cargo.toml'))
}
catch {
    throw '无法读取项目版本配置，请检查 package.json、tauri.conf.json 和 Cargo.toml。'
}

$packageSection = [regex]::Match($cargo, '(?ms)^\[package\][ \t]*\r?\n(?<body>.*?)(?=^\[|\z)')
$cargoVersion = [regex]::Match($packageSection.Groups['body'].Value, '(?m)^version\s*=\s*"(?<version>[^"\r\n]+)"\s*(?:#.*)?$').Groups['version'].Value
$version = [string]$package.version
$versionPattern = '^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$'
if ($version -cnotmatch $versionPattern) {
    throw '项目版本格式无效，必须使用可安全用于 ZIP 文件名的语义版本。'
}
if ($version -cne [string]$tauri.version -or $version -cne $cargoVersion) {
    throw 'package.json、tauri.conf.json 和 Cargo.toml 的版本不一致。'
}

try {
    $outputPath = [IO.Path]::GetFullPath($OutputDirectory)
}
catch {
    throw 'Portable 输出目录路径无效。'
}
if ([IO.File]::Exists($outputPath)) {
    throw 'Portable 输出路径不是目录。'
}
$zipName = "CC-Switch-v${version}-Windows-Portable.zip"
$zipPath = Join-Path $outputPath $zipName
if ([IO.File]::Exists($zipPath) -or [IO.Directory]::Exists($zipPath)) {
    throw 'Portable ZIP 已存在，请选择新的输出目录。'
}
$checkScript = Join-Path $PSScriptRoot 'test-portable-package.ps1'
if (-not [IO.File]::Exists($checkScript)) {
    throw 'Portable ZIP 校验脚本不存在。'
}

$workDirectory = Join-Path $outputPath ('.portable-build-' + [guid]::NewGuid().ToString('N'))
$stagingDirectory = Join-Path $workDirectory 'contents'
$temporaryZip = Join-Path $workDirectory $zipName
try {
    [IO.Directory]::CreateDirectory($stagingDirectory) | Out-Null
    [IO.Directory]::CreateDirectory((Join-Path $stagingDirectory 'data')) | Out-Null
    [IO.File]::Copy([IO.Path]::GetFullPath($ExecutablePath), (Join-Path $stagingDirectory 'cc-switch.exe'))
    [IO.File]::WriteAllText((Join-Path $stagingDirectory 'portable.ini'), '')
    [IO.File]::WriteAllText((Join-Path $stagingDirectory 'data/.gitkeep'), '')
    [IO.Compression.ZipFile]::CreateFromDirectory(
        $stagingDirectory,
        $temporaryZip,
        [IO.Compression.CompressionLevel]::Optimal,
        $false
    )
    & $checkScript -ZipPath $temporaryZip -ExpectedVersion $version
    [IO.File]::Move($temporaryZip, $zipPath)
}
catch {
    throw "Portable 打包或 ZIP 校验失败：$($_.Exception.Message)"
}
finally {
    if ([IO.Directory]::Exists($workDirectory)) {
        [IO.Directory]::Delete($workDirectory, $true)
    }
}

Write-Output $zipPath
