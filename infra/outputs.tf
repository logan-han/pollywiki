output "site_path" {
  value = "${aws_s3_bucket.main.bucket}/site"
}

output "data_path" {
  value = "${aws_s3_bucket.main.bucket}/data"
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

# Add these wherever the DNS zone is hosted.
output "dns_records_required" {
  value = concat(
    [
      for dvo in flatten([for c in aws_acm_certificate.site : c.domain_validation_options]) : {
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
