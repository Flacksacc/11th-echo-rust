$ErrorActionPreference = "Stop"

$projectRoot = Split-Path -Parent $PSScriptRoot
$manifestPath = Join-Path $projectRoot "Cargo.toml"
$outputPath = Join-Path $PSScriptRoot "licenses\RUST_THIRD_PARTY_NOTICES.txt"

$metadataJson = cargo metadata --format-version 1 --locked --manifest-path $manifestPath
if ($LASTEXITCODE -ne 0) {
    throw "Cargo metadata failed while generating third-party notices."
}
$metadata = $metadataJson | ConvertFrom-Json
$packagesById = @{}
foreach ($package in $metadata.packages) {
    $packagesById[$package.id] = $package
}

$sections = [Collections.Generic.List[string]]::new()
$sections.Add("Echo Rust dependency notices")
$sections.Add("============================")
$sections.Add("")
$sections.Add(
    "This generated inventory covers every package in Cargo's locked dependency graph. " +
    "License and notice files are reproduced from the exact package sources used for this build."
)
$sections.Add("")

$dependencyPackages = foreach ($node in $metadata.resolve.nodes) {
    if ($node.id -ne $metadata.resolve.root) {
        $packagesById[$node.id]
    }
}
$dependencyPackages = $dependencyPackages | Sort-Object name, version

foreach ($package in $dependencyPackages) {
    $sections.Add("------------------------------------------------------------------------")
    $sections.Add("$($package.name) $($package.version)")
    $sections.Add("License expression: $($package.license)")
    $sections.Add("Source: $($package.source)")
    $sections.Add("")

    $packageDirectory = Split-Path -Parent $package.manifest_path
    $licenseFiles = @()
    if ($package.license_file) {
        $declaredLicense = Join-Path $packageDirectory $package.license_file
        if (Test-Path -LiteralPath $declaredLicense -PathType Leaf) {
            $licenseFiles += Get-Item -LiteralPath $declaredLicense
        }
    }
    if ($licenseFiles.Count -eq 0) {
        $licenseFiles = @(
            Get-ChildItem -LiteralPath $packageDirectory -File |
                Where-Object {
                    $_.Name -match '^(LICENSE|LICENCE|COPYING|NOTICE)(\.|$|-)'
                } |
                Sort-Object Name
        )
    }

    if ($licenseFiles.Count -eq 0) {
        $sections.Add(
            "No standalone license file was included in the package archive; " +
            "refer to the license expression and package source above."
        )
        $sections.Add("")
        continue
    }

    foreach ($licenseFile in $licenseFiles) {
        $sections.Add("----- $($licenseFile.Name) -----")
        $licenseText = (Get-Content -LiteralPath $licenseFile.FullName -Raw).TrimEnd()
        $licenseText = [Text.RegularExpressions.Regex]::Replace(
            $licenseText,
            '[ \t]+(?=\r?$)',
            '',
            [Text.RegularExpressions.RegexOptions]::Multiline
        )
        $sections.Add($licenseText)
        $sections.Add("")
    }
}

$outputDirectory = Split-Path -Parent $outputPath
New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
[IO.File]::WriteAllText(
    $outputPath,
    ($sections -join [Environment]::NewLine).TrimEnd() + [Environment]::NewLine,
    [Text.UTF8Encoding]::new($false)
)
Write-Host "Generated $outputPath"
