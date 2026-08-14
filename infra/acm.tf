# Certificate is requested immediately so the validation CNAMEs (see outputs)
# can be added to DNS wherever the zone lives. CloudFront only attaches it once
# enable_custom_domain is flipped after issuance.
resource "aws_acm_certificate" "site" {
  provider                  = aws.us_east_1
  domain_name               = var.domains[0]
  subject_alternative_names = slice(var.domains, 1, length(var.domains))
  validation_method         = "DNS"

  lifecycle {
    create_before_destroy = true
  }
}
