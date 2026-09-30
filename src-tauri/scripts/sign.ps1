# Windows Authenticode 代码签名脚本（由 tauri bundler 经 bundle.windows.signCommand 调用）。
#
# tauri build 经 --config src-tauri/tauri.windows.conf.json 注入 signCommand 后（CI 构建与
# 本地 Windows 手动签名均适用），bundler 会对 Windows 侧所有待签产物逐个调用本脚本：
# 主程序 exe、NSIS 安装器、NSIS 卸载器等。待签文件的绝对路径通过最后一个位置参数传入
# （tauri 的 %1 占位符替换而来；相对路径参数由 bundler 相对构建 cwd 转绝对，故本脚本
# 总是收到绝对路径）。
#
# 签名走 SignPath（Foundation 开源签名，REST API 直提）：证书与策略绑定全在门户侧
# （证书挂项目下、由 signing policy 引用），CI 侧只持 API token、永不接触证书材料——
# 换正式证书仅换 SIGNPATH_SIGNING_POLICY_SLUG 策略 slug，本脚本不变。三端点内联实现
# （等价门户片段的 Submit-SigningRequest 封装，避免外部模块依赖）：
#   POST /SigningRequests/SubmitWithArtifact（multipart）→ 201 + Location 头
#   GET  {Location}/Status 轮询至终态（Completed/Failed/Denied/Canceled）
#   GET  {Location}/SignedArtifact 下载 → 回写原路径
# SignPath 的产物模型是 zip-in/zip-out：待签文件打包进 zip 提交，返回的签名产物也是
# zip（官方 github-action-submit-signing-request 对返回产物同样默认按 zip 解包），
# 故本脚本先 zip 打包、最后解包取回同名文件回写原路径。
# 回写必须是同一路径：tauri bundler 的后续产物链（安装器包含主程序 exe）与 updater
# minisign .sig 的计算都以其为准——签名必须发生在 build 过程中，不能事后补签。
#
# 注意：bundler 会吞掉本脚本的 stdout/stderr（签名失败只报 "failed to run pwsh"），
# 故失败详情额外写入 GITHUB_STEP_SUMMARY 与 RUNNER_TEMP 固定文件（workflow 的
# failure 步骤兜底打印），否则无从排查。
#
# 未配置 SIGNPATH_API_TOKEN 时输出醒目跳过日志后 exit 0，构建照常完成（secrets
# 未配置时 CI 行为与引入签名前完全一致，不会变红）。
#
# 与本脚本无关的另一套签名是 updater 的 minisign（TAURI_SIGNING_PRIVATE_KEY，
# 产 *.sig 供客户端验签更新包），两者用途不同，详见 docs/updater-signing.md 与
# docs/windows-code-signing.md。

param(
  # 待签名文件的绝对路径（tauri bundler 传入，对应 signCommand args 里的 %1）。
  [Parameter(Mandatory = $true, Position = 0)]
  [string]$TargetFile
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $TargetFile)) {
  Write-Error "签名目标不存在: $TargetFile"
  exit 1
}

# ---------------------------------------------------------------------------
# SignPath 签名（REST API 直提）
# ---------------------------------------------------------------------------
if ($env:SIGNPATH_API_TOKEN) {
  Write-Host "==> [sign.ps1] SignPath 模式签名: $TargetFile"

  # REST 标识。org ID / 项目 slug 非敏感（门户 URL 即可见），脚本内置默认值、
  # 可用环境变量覆盖；策略 slug 默认 test-signing（自签测试证书），正式证书
  # 到位后经 SIGNPATH_SIGNING_POLICY_SLUG 覆盖为 release-signing。
  $apiBase = 'https://app.signpath.io/Api/v1'
  $orgId = if ($env:SIGNPATH_ORGANIZATION_ID) { $env:SIGNPATH_ORGANIZATION_ID } else { 'c73af081-eb67-45b4-b569-fc1544b35c21' }
  $projectSlug = if ($env:SIGNPATH_PROJECT_SLUG) { $env:SIGNPATH_PROJECT_SLUG } else { 'We_Health_Tick' }
  $policySlug = if ($env:SIGNPATH_SIGNING_POLICY_SLUG) { $env:SIGNPATH_SIGNING_POLICY_SLUG } else { 'test-signing' }
  # 轮询超时（秒）。无审批策略下云端签名通常数十秒完成；若将来 release 策略
  # 启用人工审批，按审批时效经环境变量调大。
  $timeoutSeconds = if ($env:SIGNPATH_TIMEOUT_SECONDS) { [int]$env:SIGNPATH_TIMEOUT_SECONDS } else { 600 }

  $authHeader = @{ Authorization = "Bearer $($env:SIGNPATH_API_TOKEN)" }
  # Invoke-* 的进度条渲染会显著拖慢 CI 传输，关掉。
  $ProgressPreference = 'SilentlyContinue'

  # 失败详情落盘（原因见文件头注）。幂等追加，多产物失败可叠看。
  function Write-SignFailureDetail([string]$Detail) {
    try {
      if ($env:GITHUB_STEP_SUMMARY) {
        Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY `
          -Value ('#### sign.ps1 签名失败' + "`n`n" + '```' + "`n" + $Detail + "`n" + '```')
      }
      if ($env:RUNNER_TEMP) {
        Add-Content -LiteralPath (Join-Path $env:RUNNER_TEMP 'tauri-sign-error.log') -Value $Detail
      }
    } catch { }
  }

  Add-Type -AssemblyName System.IO.Compression.FileSystem
  $tempRoot = [System.IO.Path]::GetTempPath()
  $fileName = [System.IO.Path]::GetFileName($TargetFile)
  # 中间产物：打包目录 / 提交 zip / 返回 zip / 解包目录，finally 统一清理。
  $stageDir = Join-Path $tempRoot "tauri-signpath-stage-$(New-Guid)"
  $zipPath = Join-Path $tempRoot "tauri-signpath-in-$(New-Guid).zip"
  $signedZipPath = Join-Path $tempRoot "tauri-signpath-out-$(New-Guid).zip"
  $extractDir = Join-Path $tempRoot "tauri-signpath-extract-$(New-Guid)"

  try {
    # 1. 打包待签文件为 zip（文件置于 zip 根部）并提交签名请求。description 取
    #    文件名，便于门户审计列表辨认各产物（主程序 exe / 安装器 / 卸载器各一次）。
    New-Item -ItemType Directory -Path $stageDir -Force | Out-Null
    Copy-Item -LiteralPath $TargetFile -Destination (Join-Path $stageDir $fileName)
    [System.IO.Compression.ZipFile]::CreateFromDirectory($stageDir, $zipPath)

    $response = Invoke-WebRequest -Method Post `
      -Uri "$apiBase/$orgId/SigningRequests/SubmitWithArtifact" `
      -Headers $authHeader `
      -Form @{
        projectSlug = $projectSlug
        signingPolicySlug = $policySlug
        description = $fileName
        artifact = Get-Item -LiteralPath $zipPath
      }
    # 201 成功；Location 头为签名请求实体 URL（相对路径时补 API 域名）。
    $requestUrl = [string]$response.Headers.Location
    if (-not $requestUrl) {
      Write-SignFailureDetail "SubmitWithArtifact 未返回 Location 头（HTTP $($response.StatusCode)）。"
      Write-Error 'SignPath 未返回签名请求 URL（Location 响应头缺失）。'
      exit 1
    }
    if ($requestUrl.StartsWith('/')) { $requestUrl = "$apiBase$requestUrl" }
    Write-Host "==> [sign.ps1] SignPath 请求已提交: $requestUrl"

    # 2. 轮询状态至终态。WaitingForApproval 为非终态：无审批策略不会出现，
    #    策略启用审批后会一直轮询到超时（提示见下方超时报错）。
    $deadline = [DateTime]::UtcNow.AddSeconds($timeoutSeconds)
    do {
      Start-Sleep -Seconds 5
      $statusInfo = Invoke-RestMethod -Method Get -Uri "$requestUrl/Status" -Headers $authHeader
      Write-Host "==> [sign.ps1] SignPath 状态: $($statusInfo.status)"
    } until ($statusInfo.isFinalStatus -or [DateTime]::UtcNow -ge $deadline)

    if (-not $statusInfo.isFinalStatus) {
      Write-SignFailureDetail "签名请求 ${timeoutSeconds}s 内未达终态（当前状态: $($statusInfo.status)）。若策略启用了人工审批，需在门户批准；或经 SIGNPATH_TIMEOUT_SECONDS 调大超时。"
      Write-Error "SignPath 签名请求 ${timeoutSeconds}s 内未达终态（当前状态: $($statusInfo.status)）。"
      exit 1
    }
    if ($statusInfo.status -ne 'Completed') {
      Write-SignFailureDetail "签名请求终态为 $($statusInfo.status)（非 Completed）。到门户该请求详情页查看原因（凭证权限 / 策略限制 / artifact 格式问题）。"
      Write-Error "SignPath 签名请求终态为 $($statusInfo.status)（非 Completed）。"
      exit 1
    }

    # 3. 下载签名 zip，解包取回同名文件，先落解包目录再替换原路径：任一步中断都
    #    不至于损坏原始未签名产物（重跑 CI 即可恢复）。
    Invoke-WebRequest -Method Get -Uri "$requestUrl/SignedArtifact" -Headers $authHeader -OutFile $signedZipPath
    [System.IO.Compression.ZipFile]::ExtractToDirectory($signedZipPath, $extractDir)
    $signedFile = Join-Path $extractDir $fileName
    if (-not (Test-Path -LiteralPath $signedFile)) {
      $zipContent = (Get-ChildItem -LiteralPath $extractDir -Recurse | ForEach-Object { $_.FullName }) -join '; '
      Write-SignFailureDetail "签名产物 zip 中未找到 $fileName。zip 实际内容: $zipContent"
      Write-Error "SignPath 签名产物 zip 中未找到 $fileName。"
      exit 1
    }
    Move-Item -LiteralPath $signedFile -Destination $TargetFile -Force
    Write-Host "==> [sign.ps1] SignPath 签名完成，已回写: $TargetFile"
    exit 0
  }
  catch {
    # HTTP 错误响应体（如 400 的格式要求、401 的具体原因）在 ErrorDetails.Message 里，附上便于排查。
    $detail = $_.Exception.Message
    if ($_.ErrorDetails.Message) { $detail += "`n响应: $($_.ErrorDetails.Message)" }
    Write-SignFailureDetail $detail
    Write-Error "SignPath API 调用失败: $($_.Exception.Message)"
    exit 1
  }
  finally {
    foreach ($p in @($stageDir, $zipPath, $signedZipPath, $extractDir)) {
      if ($p -and (Test-Path -LiteralPath $p)) {
        Remove-Item -LiteralPath $p -Recurse -Force -ErrorAction SilentlyContinue
      }
    }
  }
}

# ---------------------------------------------------------------------------
# 未配置凭证 → 显式跳过（不构成失败）
# ---------------------------------------------------------------------------
Write-Host '==> [sign.ps1] 跳过 Windows 代码签名：未配置签名凭证（SIGNPATH_API_TOKEN）。' -ForegroundColor Yellow
Write-Host '==> [sign.ps1] 配置方法见 docs/windows-code-signing.md（SignPath API token 填入 GitHub Secrets 即自动生效）。' -ForegroundColor Yellow
exit 0
