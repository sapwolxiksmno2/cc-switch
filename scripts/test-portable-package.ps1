[CmdletBinding()]
param(
    [string]$ZipPath,
    [string]$ExpectedVersion
)

$ErrorActionPreference = 'Stop'

function Assert-Condition {
    param([bool]$Condition, [string]$Message)
    if (-not $Condition) {
        throw $Message
    }
}

function Assert-Failure {
    param([scriptblock]$Action, [string]$ExpectedMessage)
    $failure = $null
    try {
        & $Action | Out-Null
    }
    catch {
        $failure = $_
    }
    Assert-Condition ($null -ne $failure) '应当失败的操作意外成功。'
    Assert-Condition ($failure.Exception.Message.Contains($ExpectedMessage)) "失败原因不符合预期：$ExpectedMessage"
}

function Assert-PortableZip {
    param([string]$Path, [string]$Version)

    Assert-Condition ([IO.File]::Exists($Path)) 'Portable ZIP 不存在。'
    Assert-Condition ([IO.Path]::GetFileName($Path) -ceq "CC-Switch-v${Version}-Windows-Portable.zip") 'Portable ZIP 文件名与版本不一致。'
    $allowedEntries = @('cc-switch.exe', 'portable.ini', 'data/.gitkeep')
    try {
        $archive = [IO.Compression.ZipFile]::OpenRead($Path)
    }
    catch {
        throw '无法打开 Portable ZIP。'
    }
    try {
        $entries = @($archive.Entries)
        Assert-Condition ($entries.Count -eq $allowedEntries.Count) 'Portable ZIP 必须仅包含三个白名单文件。'
        foreach ($name in $allowedEntries) {
            $matchingEntries = @($entries | Where-Object { $_.FullName -ceq $name })
            Assert-Condition ($matchingEntries.Count -eq 1) "Portable ZIP 缺少白名单文件或包含重复条目：$name"
        }
        Assert-Condition ($archive.GetEntry('cc-switch.exe').Length -gt 0) 'Portable EXE 不能为空。'
        Assert-Condition ($archive.GetEntry('portable.ini').Length -eq 0) 'Portable 标记必须为空文件。'
        Assert-Condition ($archive.GetEntry('data/.gitkeep').Length -eq 0) 'Portable data 必须只包含空占位文件。'
    }
    finally {
        $archive.Dispose()
    }

    $extracted = Join-Path ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($Path))) ('.portable-check-' + [guid]::NewGuid().ToString('N'))
    try {
        [IO.Directory]::CreateDirectory($extracted) | Out-Null
        try {
            [IO.Compression.ZipFile]::ExtractToDirectory([IO.Path]::GetFullPath($Path), $extracted)
        }
        catch {
            throw '无法解压 Portable ZIP。'
        }
        Assert-Condition ([IO.File]::Exists((Join-Path $extracted 'cc-switch.exe'))) '解压根目录缺少 EXE。'
        Assert-Condition ([IO.File]::Exists((Join-Path $extracted 'portable.ini'))) '解压根目录缺少 Portable 标记。'
        Assert-Condition ([IO.Directory]::Exists((Join-Path $extracted 'data'))) '解压根目录缺少 data。'
        Assert-Condition ([IO.File]::Exists((Join-Path $extracted 'data/.gitkeep'))) '解压后缺少 data/.gitkeep。'
        Assert-Condition ([IO.Directory]::GetFileSystemEntries($extracted).Count -eq 3) '解压根目录包含额外目录或文件。'
    }
    finally {
        if ([IO.Directory]::Exists($extracted)) {
            [IO.Directory]::Delete($extracted, $true)
        }
    }
}

if ($PSBoundParameters.ContainsKey('ZipPath')) {
    Assert-Condition (-not [string]::IsNullOrWhiteSpace($ExpectedVersion)) '校验 ZIP 时必须提供 ExpectedVersion。'
    Assert-PortableZip -Path $ZipPath -Version $ExpectedVersion
    Write-Host 'Portable ZIP 文件名、白名单和解压结构校验通过。'
    return
}

Assert-Condition (-not $PSBoundParameters.ContainsKey('ExpectedVersion')) 'ExpectedVersion 必须与 ZipPath 一起使用。'
$buildScript = Join-Path $PSScriptRoot 'build-portable.ps1'
Assert-Condition ([IO.File]::Exists($buildScript)) '打包脚本尚未实现。'
$repositoryRoot = Split-Path $PSScriptRoot -Parent
$fixtureRoot = Join-Path (Split-Path $repositoryRoot -Parent) ('.portable-package-test-' + [guid]::NewGuid().ToString('N'))
$fixtureProject = Join-Path $fixtureRoot '项目 夹具'
$tauriDirectory = Join-Path $fixtureProject 'src-tauri'
$executable = Join-Path $fixtureRoot '输入 中文/伪程序 含空格.exe'
$outputDirectory = Join-Path $fixtureRoot '输出 中文/Portable 成品'

function Set-FixtureVersion {
    param([string]$Version, [string]$TauriVersion = $Version)
    [IO.File]::WriteAllText((Join-Path $fixtureProject 'package.json'), (@{ version = $Version } | ConvertTo-Json -Compress))
    [IO.File]::WriteAllText((Join-Path $tauriDirectory 'tauri.conf.json'), (@{ version = $TauriVersion } | ConvertTo-Json -Compress))
    [IO.File]::WriteAllText((Join-Path $tauriDirectory 'Cargo.toml'), "[package]`nname = `"fixture`"`nversion = `"$Version`"`n`n[dependencies]`n")
}

try {
    [IO.Directory]::CreateDirectory($tauriDirectory) | Out-Null
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($executable)) | Out-Null
    [IO.Directory]::CreateDirectory($outputDirectory) | Out-Null
    [byte[]]$exeBytes = @(0x4d, 0x5a, 0x00, 0x01)
    [IO.File]::WriteAllBytes($executable, $exeBytes)
    $unrelatedFile = Join-Path $outputDirectory '用户数据 夹具.json'
    [IO.File]::WriteAllText($unrelatedFile, '{}')
    Set-FixtureVersion '5.6.7'

    $result = & $buildScript -ExecutablePath $executable -OutputDirectory $outputDirectory -ProjectRoot $fixtureProject
    $expectedZip = Join-Path $outputDirectory 'CC-Switch-v5.6.7-Windows-Portable.zip'
    Assert-Condition ($result -ceq $expectedZip) '打包脚本必须返回最终单个 ZIP 路径。'
    Assert-PortableZip -Path $result -Version '5.6.7'
    Assert-Condition ([IO.File]::ReadAllText($unrelatedFile) -ceq '{}') '打包操作修改了已有输出目录中的其他文件。'
    Assert-Condition ([IO.Directory]::GetFileSystemEntries($outputDirectory).Count -eq 2) '打包操作遗留了 staging 或临时 ZIP。'
    $archive = [IO.Compression.ZipFile]::OpenRead($result)
    try {
        $stream = $archive.GetEntry('cc-switch.exe').Open()
        try {
            $memory = [IO.MemoryStream]::new()
            $stream.CopyTo($memory)
            Assert-Condition ([Convert]::ToHexString($memory.ToArray()) -ceq '4D5A0001') '打包操作改变了 EXE 内容。'
        }
        finally {
            $stream.Dispose()
            if ($null -ne $memory) { $memory.Dispose() }
        }
    }
    finally {
        $archive.Dispose()
    }
    Write-Host '通过：中文和空格路径、三文件白名单、隐藏占位文件、EXE 内容及临时文件清理。'

    $originalHash = (Get-FileHash -LiteralPath $result -Algorithm SHA256).Hash
    Assert-Failure { & $buildScript -ExecutablePath $executable -OutputDirectory $outputDirectory -ProjectRoot $fixtureProject } 'ZIP 已存在'
    Assert-Condition ((Get-FileHash -LiteralPath $result -Algorithm SHA256).Hash -ceq $originalHash) '失败操作覆盖了已有 ZIP。'
    Write-Host '通过：已有 ZIP 拒绝覆盖。'

    Set-FixtureVersion '8.9.10-rc.1'
    $futureOutput = Join-Path $fixtureRoot '下一版本'
    $futureZip = & $buildScript -ExecutablePath $executable -OutputDirectory $futureOutput -ProjectRoot $fixtureProject
    Assert-Condition ([IO.Path]::GetFileName($futureZip) -ceq 'CC-Switch-v8.9.10-rc.1-Windows-Portable.zip') 'ZIP 名称未跟随项目版本变化。'
    Assert-PortableZip -Path $futureZip -Version '8.9.10-rc.1'
    Write-Host '通过：版本自动读取，包括预发布版本。'

    $missingOutput = Join-Path $fixtureRoot '不应创建的目录'
    Assert-Failure { & $buildScript -ExecutablePath (Join-Path $fixtureRoot 'missing.exe') -OutputDirectory $missingOutput -ProjectRoot $fixtureProject } '可执行文件不存在'
    Assert-Condition (-not [IO.Directory]::Exists($missingOutput)) '缺少 EXE 时不应创建输出目录。'
    Assert-Failure { & $buildScript -ExecutablePath $fixtureProject -OutputDirectory $missingOutput -ProjectRoot $fixtureProject } '可执行文件不存在'
    $emptyExecutable = Join-Path $fixtureRoot '空程序.exe'
    [IO.File]::WriteAllText($emptyExecutable, '')
    Assert-Failure { & $buildScript -ExecutablePath $emptyExecutable -OutputDirectory $missingOutput -ProjectRoot $fixtureProject } '可执行文件不能为空'
    Write-Host '通过：缺少 EXE、空 EXE 或把目录作为 EXE 时清晰失败。'

    Set-FixtureVersion -Version '8.9.10' -TauriVersion '8.9.11'
    Assert-Failure { & $buildScript -ExecutablePath $executable -OutputDirectory $missingOutput -ProjectRoot $fixtureProject } '版本不一致'
    Assert-Condition (-not [IO.Directory]::Exists($missingOutput)) '版本不一致时不应创建输出目录。'
    Set-FixtureVersion '8.9.10'
    [IO.File]::WriteAllText((Join-Path $tauriDirectory 'Cargo.toml'), "[package]`nname = `"fixture`"`nversion = `"8.9.11`"`n")
    Assert-Failure { & $buildScript -ExecutablePath $executable -OutputDirectory $missingOutput -ProjectRoot $fixtureProject } '版本不一致'
    Set-FixtureVersion '../invalid'
    Assert-Failure { & $buildScript -ExecutablePath $executable -OutputDirectory $missingOutput -ProjectRoot $fixtureProject } '版本格式'
    Write-Host '通过：拒绝 Tauri 或 Cargo 版本不一致以及不安全的版本名称。'

    Set-FixtureVersion '5.6.7'
    Assert-Failure { & $buildScript -ExecutablePath $executable -OutputDirectory $unrelatedFile -ProjectRoot $fixtureProject } '输出路径不是目录'
    Write-Host '通过：输出路径为文件时清晰失败。'

    $invalidStaging = Join-Path $fixtureRoot '故意缺少占位文件'
    $invalidOutput = Join-Path $fixtureRoot '无效 ZIP'
    [IO.Directory]::CreateDirectory($invalidStaging) | Out-Null
    [IO.Directory]::CreateDirectory($invalidOutput) | Out-Null
    [IO.File]::WriteAllBytes((Join-Path $invalidStaging 'cc-switch.exe'), $exeBytes)
    [IO.File]::WriteAllText((Join-Path $invalidStaging 'portable.ini'), '')
    $invalidZip = Join-Path $invalidOutput 'CC-Switch-v5.6.7-Windows-Portable.zip'
    [IO.Compression.ZipFile]::CreateFromDirectory($invalidStaging, $invalidZip, [IO.Compression.CompressionLevel]::Optimal, $false)
    Assert-Failure { Assert-PortableZip -Path $invalidZip -Version '5.6.7' } '三个白名单文件'
    [IO.File]::Delete($invalidZip)
    [IO.Directory]::CreateDirectory((Join-Path $invalidStaging 'data')) | Out-Null
    [IO.File]::WriteAllText((Join-Path $invalidStaging 'data/.gitkeep'), '')
    [IO.File]::WriteAllText((Join-Path $invalidStaging 'data/用户数据.json'), '{}')
    [IO.Compression.ZipFile]::CreateFromDirectory($invalidStaging, $invalidZip, [IO.Compression.CompressionLevel]::Optimal, $false)
    Assert-Failure { Assert-PortableZip -Path $invalidZip -Version '5.6.7' } '三个白名单文件'
    [IO.File]::Delete($invalidZip)
    [IO.File]::Delete((Join-Path $invalidStaging 'data/用户数据.json'))
    [IO.Compression.ZipFile]::CreateFromDirectory($invalidStaging, $invalidZip, [IO.Compression.CompressionLevel]::Optimal, $true)
    Assert-Failure { Assert-PortableZip -Path $invalidZip -Version '5.6.7' } '缺少白名单文件'
    [IO.File]::Delete($invalidZip)
    [IO.File]::Copy($result, (Join-Path $invalidStaging 'nested.zip'))
    [IO.Compression.ZipFile]::CreateFromDirectory($invalidStaging, $invalidZip, [IO.Compression.CompressionLevel]::Optimal, $false)
    Assert-Failure { Assert-PortableZip -Path $invalidZip -Version '5.6.7' } '三个白名单文件'
    Write-Host '通过：拒绝缺少 data/.gitkeep、夹带用户数据、额外顶层目录或嵌套 ZIP。'

    Assert-Failure { Assert-PortableZip -Path $result -Version '0.0.1' } '文件名与版本不一致'
    Write-Host 'Portable 打包测试全部通过；夹具 EXE 从未执行。'
}
finally {
    if ([IO.Directory]::Exists($fixtureRoot)) {
        [IO.Directory]::Delete($fixtureRoot, $true)
    }
}
