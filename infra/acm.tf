# Only created when no existing certificate is supplied. Validation CNAMEs
# (see outputs) must be added to DNS wherever the zone lives; CloudFront can
# only attach the certificate once it is issued.
resource "aws_acm_certificate" "site" {
  count = var.acm_certificate_arn == "" ? 1 : 0

  provider                  = aws.us_east_1
  domain_name               = var.domains[0]
  subject_alternative_names = slice(var.domains, 1, length(var.domains))
  validation_method         = "DNS"

  lifecycle {
    create_before_destroy = true
  }
}

locals {
  certificate_arn = var.acm_certificate_arn != "" ? var.acm_certificate_arn : aws_acm_certificate.site[0].arn
}
