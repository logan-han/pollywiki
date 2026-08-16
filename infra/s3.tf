data "aws_caller_identity" "current" {}

locals {
  account_id = data.aws_caller_identity.current.account_id
}

# Everything lives in one bucket: the published site under site/, the data
# store under data/. Terraform state stays in its own bucket on purpose, so
# state can never be clobbered by a site sync or a bucket destroy.
resource "aws_s3_bucket" "main" {
  bucket = "pollywiki.au"
}

resource "aws_s3_bucket_versioning" "main" {
  bucket = aws_s3_bucket.main.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_public_access_block" "main" {
  bucket                  = aws_s3_bucket.main.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_policy" "main" {
  bucket = aws_s3_bucket.main.id
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid       = "AllowCloudFrontOAC"
      Effect    = "Allow"
      Principal = { Service = "cloudfront.amazonaws.com" }
      Action    = "s3:GetObject"
      Resource  = "${aws_s3_bucket.main.arn}/site/*"
      Condition = {
        StringEquals = { "AWS:SourceArn" = aws_cloudfront_distribution.site.arn }
      }
    }]
  })
}

resource "aws_s3_bucket_lifecycle_configuration" "main" {
  bucket = aws_s3_bucket.main.id

  # Raw source payloads cool down; canonical and bundles stay hot.
  rule {
    id     = "raw-to-ia"
    status = "Enabled"

    filter {
      prefix = "data/raw/"
    }

    transition {
      days          = 90
      storage_class = "STANDARD_IA"
    }
  }

  # Every deploy replaces the site and every sync rewrites bundles; old
  # versions only need to stick around long enough to undo a mistake.
  rule {
    id     = "trim-noncurrent"
    status = "Enabled"

    filter {}

    noncurrent_version_expiration {
      noncurrent_days = 60
    }
  }
}

