[CmdletBinding()]
param(
    [string]$DependencyRoot,
    [string]$CompilerPrefix = 'C:\msys64\mingw64',
    [string]$BuildDirectory
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$RepositoryRoot = [System.IO.Path]::GetFullPath(
    (Join-Path $PSScriptRoot '..\..')
)
if ([string]::IsNullOrWhiteSpace($DependencyRoot)) {
    $DependencyRoot = Join-Path $RepositoryRoot 'target\benchmark-deps'
}
if ([string]::IsNullOrWhiteSpace($BuildDirectory)) {
    $BuildDirectory = Join-Path $DependencyRoot 'build\resampler-shims'
}

$DependencyRoot = [System.IO.Path]::GetFullPath($DependencyRoot)
$BuildDirectory = [System.IO.Path]::GetFullPath($BuildDirectory)
$CompilerPrefix = [System.IO.Path]::GetFullPath($CompilerPrefix)
$CompilerBin = Join-Path $CompilerPrefix 'bin'
$Gxx = Join-Path $CompilerBin 'g++.exe'
$Gcc = Join-Path $CompilerBin 'gcc.exe'
$ShimRoot = Join-Path $PSScriptRoot 'resampler_shims'

function Assert-File {
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Required file is missing: $Path"
    }
}

function Assert-Directory {
    param([Parameter(Mandatory)][string]$Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        throw "Required directory is missing: $Path"
    }
}

function Assert-Sha256 {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Expected
    )
    Assert-File $Path
    $Actual = (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash
    if ($Actual -ne $Expected) {
        throw "SHA-256 mismatch for ${Path}: expected $Expected, got $Actual"
    }
}

function Get-TreeManifestSha256 {
    param([Parameter(Mandatory)][string]$Path)
    Assert-Directory $Path
    $Root = [System.IO.Path]::GetFullPath($Path)
    $Files = @(
        Get-ChildItem -LiteralPath $Root -Recurse -File |
            Sort-Object { [System.IO.Path]::GetRelativePath($Root, $_.FullName) }
    )
    if ($Files.Count -eq 0) {
        throw "Cannot hash an empty file tree: $Root"
    }
    $Manifest = @(
        foreach ($File in $Files) {
            $RelativePath = [System.IO.Path]::GetRelativePath($Root, $File.FullName).Replace('\', '/')
            $FileHash = (Get-FileHash -LiteralPath $File.FullName -Algorithm SHA256).Hash
            "$RelativePath $FileHash"
        }
    )
    $Utf8NoBom = [System.Text.UTF8Encoding]::new($false)
    $ManifestBytes = $Utf8NoBom.GetBytes([string]::Join("`n", $Manifest))
    return [System.Convert]::ToHexString(
        [System.Security.Cryptography.SHA256]::HashData($ManifestBytes)
    )
}

function Assert-TreeManifestSha256 {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Expected
    )
    $Actual = Get-TreeManifestSha256 $Path
    if ($Actual -ne $Expected) {
        throw "Tree manifest SHA-256 mismatch for ${Path}: expected $Expected, got $Actual"
    }
}

function Assert-GitRevision {
    param(
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Expected
    )
    Assert-Directory $Path
    $GitSafePath = $Path.Replace('\', '/')
    $RevisionOutput = & git -c "safe.directory=$GitSafePath" -C $Path rev-parse HEAD
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to read git revision for $Path"
    }
    $Actual = ($RevisionOutput | Select-Object -First 1).Trim()
    if ($Actual -ne $Expected) {
        throw "Git revision mismatch for ${Path}: expected $Expected, got $Actual"
    }
    $StatusOutput = @(
        & git -c "safe.directory=$GitSafePath" -C $Path status --porcelain=v1 --untracked-files=all
    )
    if ($LASTEXITCODE -ne 0) {
        throw "Failed to inspect git worktree state for $Path"
    }
    if ($StatusOutput.Count -ne 0) {
        $Summary = ($StatusOutput | Select-Object -First 5) -join '; '
        throw "Pinned git worktree is not clean at ${Path}: $Summary"
    }
}

function Invoke-NativeTool {
    param(
        [Parameter(Mandatory)][string]$Tool,
        [Parameter(Mandatory)][string[]]$ToolArguments,
        [Parameter(Mandatory)][string]$Label
    )
    Write-Host "Building $Label"
    & $Tool @ToolArguments
    if ($LASTEXITCODE -ne 0) {
        throw "$Label failed with exit code $LASTEXITCODE"
    }
}

Assert-File $Gxx
Assert-File $Gcc
Assert-Directory $DependencyRoot
New-Item -ItemType Directory -Path $BuildDirectory -Force | Out-Null

$FfmpegSource = Join-Path $DependencyRoot 'src\FFmpeg-n8.0.1'
$FfmpegInstall = Join-Path $DependencyRoot 'build\ffmpeg-n8.0.1-minimal'
$SpeexRoot = Join-Path $DependencyRoot 'extracted\speexdsp-1.2.1-1\mingw64'
$R8brainRoot = Join-Path $DependencyRoot 'src\r8brain-free-src'
$ZitaRoot = Join-Path $DependencyRoot 'src\zita-resampler-1.11.2\source'
$WebRtcRoot = Join-Path $DependencyRoot 'src\webrtc-audio-processing-v1.3'
$WdlRoot = Join-Path $DependencyRoot 'src\WDL'
$LibresampleRoot = Join-Path $DependencyRoot 'src\libresample'

Assert-GitRevision $FfmpegSource '894da5ca7d742e4429ffb2af534fcda0103ef593'
Assert-GitRevision $R8brainRoot 'e71c31bf320f84210bb4bdcb57e296c39ce940f9'
Assert-GitRevision $WebRtcRoot '8e258a1933d405073c9e6465628a69ac7d2a1f13'
Assert-GitRevision $WdlRoot '96b770f7368f75b53756e0c8941ce3ecc8b6c29b'
Assert-GitRevision $LibresampleRoot '7cb7f9c3f72d4e6774d964dc324af827192df7c3'
Assert-Sha256 `
    (Join-Path $DependencyRoot 'packages\mingw-w64-x86_64-speexdsp-1.2.1-1-any.pkg.tar.zst') `
    'E46B80E43DB1436F9469FB9500FEB1A0D3879E63B35703BABA6F46AF4949A4C8'
Assert-Sha256 `
    (Join-Path $DependencyRoot 'packages\zita-resampler-1.11.2.tar.xz') `
    'AA5C54E696069AF26F3F1FED4A963113CC1237CDDFD57AE5842ABCB1ACD5492C'

$FfmpegInclude = Join-Path $FfmpegInstall 'include'
$FfmpegLib = Join-Path $FfmpegInstall 'lib'
$FfmpegBin = Join-Path $FfmpegInstall 'bin'
Assert-TreeManifestSha256 `
    $FfmpegInclude `
    '9C7EF81AF2DA1EEA17A5C5EAA3A678BD72F5C0F70C99C4A22D4C064D43666AFA'
Assert-Sha256 `
    (Join-Path $FfmpegLib 'libswresample.dll.a') `
    '3D5062996390328A34A3E4DE82CB1D97281622CB6AF722BFCCD25C0D503026A7'
Assert-Sha256 `
    (Join-Path $FfmpegLib 'libavutil.dll.a') `
    '45F94EDDB106704164994C957BC05BD46288723D0AC97FEBA18C35AFBE2E4E7F'
Assert-Sha256 `
    (Join-Path $FfmpegBin 'swresample-6.dll') `
    '98FDB4D1788BD64C2282C7B363D5675F3844DD8BFCD8BBF27690C5595A63F0C7'
Assert-Sha256 `
    (Join-Path $FfmpegBin 'avutil-60.dll') `
    '6D025CF39A586811EB4F8944C364DF507504CAE567F491A7A5A4FD17443BAA3E'
Assert-Sha256 `
    (Join-Path $SpeexRoot 'lib\libspeexdsp.dll.a') `
    '0E1749DA0F497E21BD3A1D61F15A46B61CFBB825D5CD23318DF6C41AA4A71536'
Assert-Sha256 `
    (Join-Path $SpeexRoot 'bin\libspeexdsp-1.dll') `
    '676DE283408C6A7C06221774BBF8150DCFE94668E0A249D67E81EDE17CC22A45'
Assert-Sha256 `
    (Join-Path $CompilerBin 'libwinpthread-1.dll') `
    'B0D84F7B6346CF835EF19ECC95991CDAA6272BB8AD6FEE43F446C07AA97FCBD9'

$PreviousPath = $env:Path
try {
    $env:Path = "$CompilerBin;$PreviousPath"
    $CompilerVersion = (& $Gxx --version | Select-Object -First 1)
    if ($LASTEXITCODE -ne 0 -or $CompilerVersion -notmatch '15\.2\.0') {
        throw "Expected MinGW-w64 GCC 15.2.0, got '$CompilerVersion'"
    }

    $CppRelease = @('-std=c++17', '-O3', '-DNDEBUG')
    $CppV2 = $CppRelease + @('-march=x86-64-v2')
    $SharedRuntime = @('-shared', '-static-libgcc', '-static-libstdc++')

    Invoke-NativeTool $Gxx (
        $CppRelease +
        @(
            "-I$ShimRoot",
            "-I$FfmpegInclude",
            (Join-Path $ShimRoot 'ffmpeg_libswresample_shim.cpp'),
            "-L$FfmpegLib",
            '-lswresample',
            '-lavutil'
        ) +
        $SharedRuntime +
        @('-o', (Join-Path $BuildDirectory 'ffmpeg_libswresample_shim.dll'))
    ) 'FFmpeg libswresample shim'

    $SpeexInclude = Join-Path $SpeexRoot 'include'
    $SpeexLib = Join-Path $SpeexRoot 'lib'
    Assert-Directory $SpeexInclude
    Assert-Directory $SpeexLib
    Invoke-NativeTool $Gxx (
        $CppRelease +
        @(
            "-I$ShimRoot",
            "-I$SpeexInclude",
            (Join-Path $ShimRoot 'speexdsp_shim.cpp'),
            "-L$SpeexLib",
            '-lspeexdsp'
        ) +
        $SharedRuntime +
        @('-o', (Join-Path $BuildDirectory 'speexdsp_shim.dll'))
    ) 'SpeexDSP shim'

    Invoke-NativeTool $Gxx (
        $CppV2 +
        @(
            "-I$ShimRoot",
            "-I$R8brainRoot",
            (Join-Path $ShimRoot 'r8brain_shim.cpp')
        ) +
        $SharedRuntime +
        @('-o', (Join-Path $BuildDirectory 'r8brain_shim.dll'))
    ) 'r8brain shim'

    $ZitaSource = Join-Path $ZitaRoot 'source'
    $ZitaCompat = Join-Path $ShimRoot 'zita_mingw_compat.h'
    $ZitaFlags = $CppRelease + @('-DENABLE_SSE2', '-msse3', '-include', $ZitaCompat)
    Invoke-NativeTool $Gxx (
        $ZitaFlags +
        @(
            "-I$ZitaSource",
            '-c',
            (Join-Path $ZitaSource 'resampler.cc'),
            '-o',
            (Join-Path $BuildDirectory 'zita_resampler.o')
        )
    ) 'zita Resampler object'
    Invoke-NativeTool $Gxx (
        $ZitaFlags +
        @(
            "-I$ZitaSource",
            '-c',
            (Join-Path $ZitaSource 'resampler-table.cc'),
            '-o',
            (Join-Path $BuildDirectory 'zita_resampler_table.o')
        )
    ) 'zita filter-table object'
    Invoke-NativeTool $Gxx (
        $ZitaFlags +
        @(
            "-I$ShimRoot",
            "-I$ZitaSource",
            '-c',
            (Join-Path $ShimRoot 'zita_resampler_shim.cpp'),
            '-o',
            (Join-Path $BuildDirectory 'zita_shim.o')
        )
    ) 'zita shim object'
    Invoke-NativeTool $Gxx (
        @(
            (Join-Path $BuildDirectory 'zita_shim.o'),
            (Join-Path $BuildDirectory 'zita_resampler.o'),
            (Join-Path $BuildDirectory 'zita_resampler_table.o')
        ) +
        $SharedRuntime +
        @('-o', (Join-Path $BuildDirectory 'zita_resampler_shim.dll'))
    ) 'zita shared shim'

    $WdlSource = Join-Path $WdlRoot 'WDL'
    Invoke-NativeTool $Gxx (
        $CppV2 +
        @(
            "-I$ShimRoot",
            "-I$WdlSource",
            (Join-Path $ShimRoot 'wdl_shim.cpp'),
            (Join-Path $WdlSource 'resample.cpp')
        ) +
        $SharedRuntime +
        @('-o', (Join-Path $BuildDirectory 'wdl_shim.dll'))
    ) 'WDL shim'

    $LibresampleInclude = Join-Path $LibresampleRoot 'include'
    $LibresampleSource = Join-Path $LibresampleRoot 'src'
    $LibresampleCFlags = @(
        '-O3',
        '-DNDEBUG',
        '-march=x86-64-v2',
        '-DWIN32',
        '-DHAVE_INTTYPES_H=1',
        "-I$LibresampleInclude",
        "-I$LibresampleSource"
    )
    foreach ($SourceName in @('resample.c', 'filterkit.c', 'resamplesubs.c')) {
        $ObjectName = 'libresample_' + [System.IO.Path]::GetFileNameWithoutExtension($SourceName) + '.o'
        Invoke-NativeTool $Gcc (
            $LibresampleCFlags +
            @(
                '-c',
                (Join-Path $LibresampleSource $SourceName),
                '-o',
                (Join-Path $BuildDirectory $ObjectName)
            )
        ) "libresample $SourceName object"
    }
    Invoke-NativeTool $Gxx (
        $CppV2 +
        @(
            "-I$ShimRoot",
            "-I$LibresampleInclude",
            '-c',
            (Join-Path $ShimRoot 'libresample_shim.cpp'),
            '-o',
            (Join-Path $BuildDirectory 'libresample_shim.o')
        )
    ) 'libresample shim object'
    Invoke-NativeTool $Gxx (
        @(
            (Join-Path $BuildDirectory 'libresample_shim.o'),
            (Join-Path $BuildDirectory 'libresample_resample.o'),
            (Join-Path $BuildDirectory 'libresample_filterkit.o'),
            (Join-Path $BuildDirectory 'libresample_resamplesubs.o')
        ) +
        $SharedRuntime +
        @('-o', (Join-Path $BuildDirectory 'libresample_shim.dll'))
    ) 'libresample shared shim'

    $WebRtcSource = Join-Path $WebRtcRoot 'webrtc'
    $WebRtcCompat = Join-Path $ShimRoot 'webrtc_checks_compat.h'
    $WebRtcFlags = $CppV2 + @(
        '-DWEBRTC_WIN',
        '-D_WIN32',
        '-D__STDC_FORMAT_MACROS=1',
        '-DNOMINMAX',
        '-DWEBRTC_LIBRARY_IMPL',
        '-DWEBRTC_ENABLE_SYMBOL_EXPORT',
        '-DWEBRTC_ENABLE_AVX2',
        '-include',
        $WebRtcCompat,
        "-I$ShimRoot",
        "-I$WebRtcSource"
    )
    $WebRtcObjects = [ordered]@{
        'webrtc_shim.o' = Join-Path $ShimRoot 'webrtc_shim.cpp'
        'webrtc_audio_util.o' = Join-Path $WebRtcSource 'common_audio\audio_util.cc'
        'webrtc_push_resampler.o' = Join-Path $WebRtcSource 'common_audio\resampler\push_resampler.cc'
        'webrtc_push_sinc_resampler.o' = Join-Path $WebRtcSource 'common_audio\resampler\push_sinc_resampler.cc'
        'webrtc_sinc_resampler.o' = Join-Path $WebRtcSource 'common_audio\resampler\sinc_resampler.cc'
    }
    foreach ($Entry in $WebRtcObjects.GetEnumerator()) {
        Invoke-NativeTool $Gxx (
            $WebRtcFlags + @('-c', $Entry.Value, '-o', (Join-Path $BuildDirectory $Entry.Key))
        ) "WebRTC $($Entry.Key)"
    }
    Invoke-NativeTool $Gxx (
        $WebRtcFlags +
        @(
            '-msse2',
            '-c',
            (Join-Path $WebRtcSource 'common_audio\resampler\sinc_resampler_sse.cc'),
            '-o',
            (Join-Path $BuildDirectory 'webrtc_sinc_resampler_sse.o')
        )
    ) 'WebRTC SSE2 convolution object'
    Invoke-NativeTool $Gxx (
        $WebRtcFlags +
        @(
            '-mavx2',
            '-mfma',
            '-c',
            (Join-Path $WebRtcSource 'common_audio\resampler\sinc_resampler_avx2.cc'),
            '-o',
            (Join-Path $BuildDirectory 'webrtc_sinc_resampler_avx2.o')
        )
    ) 'WebRTC AVX2 convolution object'
    Invoke-NativeTool $Gxx (
        @(
            (Join-Path $BuildDirectory 'webrtc_shim.o'),
            (Join-Path $BuildDirectory 'webrtc_audio_util.o'),
            (Join-Path $BuildDirectory 'webrtc_push_resampler.o'),
            (Join-Path $BuildDirectory 'webrtc_push_sinc_resampler.o'),
            (Join-Path $BuildDirectory 'webrtc_sinc_resampler.o'),
            (Join-Path $BuildDirectory 'webrtc_sinc_resampler_sse.o'),
            (Join-Path $BuildDirectory 'webrtc_sinc_resampler_avx2.o')
        ) +
        $SharedRuntime +
        @('-o', (Join-Path $BuildDirectory 'webrtc_shim.dll'))
    ) 'WebRTC shared shim'

    Copy-Item -LiteralPath (Join-Path $FfmpegInstall 'bin\swresample-6.dll') `
        -Destination $BuildDirectory -Force
    Copy-Item -LiteralPath (Join-Path $FfmpegInstall 'bin\avutil-60.dll') `
        -Destination $BuildDirectory -Force
    Copy-Item -LiteralPath (Join-Path $SpeexRoot 'bin\libspeexdsp-1.dll') `
        -Destination $BuildDirectory -Force
    Copy-Item -LiteralPath (Join-Path $CompilerBin 'libwinpthread-1.dll') `
        -Destination $BuildDirectory -Force

    $Artifacts = @(
        'ffmpeg_libswresample_shim.dll',
        'speexdsp_shim.dll',
        'r8brain_shim.dll',
        'zita_resampler_shim.dll',
        'webrtc_shim.dll',
        'wdl_shim.dll',
        'libresample_shim.dll'
    )
    foreach ($Artifact in $Artifacts) {
        $ArtifactPath = Join-Path $BuildDirectory $Artifact
        Assert-File $ArtifactPath
        $Hash = (Get-FileHash -LiteralPath $ArtifactPath -Algorithm SHA256).Hash
        $Bytes = (Get-Item -LiteralPath $ArtifactPath).Length
        Write-Host "$Artifact sha256=$Hash bytes=$Bytes"
    }
}
finally {
    $env:Path = $PreviousPath
}
