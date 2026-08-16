# GitHub Actions self-hosted runner on CodeBuild Lambda compute, used for
# sources that refuse non-Australian traffic (They Vote For You). Lambda
# compute bills per second with no idle cost; the AU job runs a few minutes
# nightly, which rounds to roughly ten cents a month.

variable "github_runner_pat" {
  type      = string
  sensitive = true
  default   = ""
  description = "GitHub PAT (repo scope) for the CodeBuild runner webhook; set via TF_VAR_github_runner_pat."
}

resource "aws_codebuild_source_credential" "github" {
  count       = var.github_runner_pat == "" ? 0 : 1
  auth_type   = "PERSONAL_ACCESS_TOKEN"
  server_type = "GITHUB"
  token       = var.github_runner_pat
}

resource "aws_iam_role" "codebuild_runner" {
  name = "${var.project}-codebuild-runner"
  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow"
      Principal = { Service = "codebuild.amazonaws.com" }
      Action    = "sts:AssumeRole"
    }]
  })
}

resource "aws_iam_role_policy" "codebuild_runner" {
  name = "logs"
  role = aws_iam_role.codebuild_runner.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect = "Allow"
      Action = ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"]
      Resource = ["arn:aws:logs:${var.region}:${local.account_id}:log-group:/aws/codebuild/${var.project}-gha-runner*"]
    }]
  })
}

resource "aws_codebuild_project" "gha_runner" {
  name          = "${var.project}-gha-runner"
  description   = "GitHub Actions runner with Australian egress for pollywiki ingest"
  service_role  = aws_iam_role.codebuild_runner.arn
  build_timeout = 15

  artifacts {
    type = "NO_ARTIFACTS"
  }

  environment {
    compute_type = "BUILD_LAMBDA_1GB"
    type         = "ARM_LAMBDA_CONTAINER"
    image        = "aws/codebuild/amazonlinux-aarch64-lambda-standard:nodejs22"
  }

  source {
    type     = "GITHUB"
    location = "https://github.com/${var.github_repository}.git"
  }

  depends_on = [aws_codebuild_source_credential.github]
}

# Turns the project into an Actions runner: CodeBuild starts a build for each
# queued workflow job that targets runs-on: codebuild-<project>-...
resource "aws_codebuild_webhook" "gha_runner" {
  project_name = aws_codebuild_project.gha_runner.name
  build_type   = "BUILD"

  filter_group {
    filter {
      type    = "EVENT"
      pattern = "WORKFLOW_JOB_QUEUED"
    }
  }
}
