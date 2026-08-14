output "site_bucket" {
  value = aws_s3_bucket.site.bucket
}

output "data_bucket" {
  value = aws_s3_bucket.data.bucket
}

output "cloudfront_distribution_id" {
  value = aws_cloudfront_distribution.site.id
}

output "cloudfront_domain" {
  value = aws_cloudfront_distribution.site.domain_name
}

output "deploy_role_arn" {
  value = aws_iam_role.deploy.arn
}

output "ingest_role_arn" {
  value = aws_iam_role.ingest.arn
}

# Add these wherever the DNS zone is hosted, then flip enable_custom_domain.
output "dns_records_required" {
  value = concat(
    [
      for dvo in aws_acm_certificate.site.domain_validation_options : {
        purpose = "ACM validation for ${dvo.domain_name}"
        type    = dvo.resource_record_type
        name    = dvo.resource_record_name
        value   = dvo.resource_record_value
      }
    ],
    [
      for domain in var.domains : {
        purpose = "Point ${domain} at the site"
        type    = "CNAME"
        name    = "${domain}."
        value   = "${aws_cloudfront_distribution.site.domain_name}."
      }
    ],
  )
}
