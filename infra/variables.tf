variable "project" {
  type    = string
  default = "pollywiki"
}

variable "region" {
  type    = string
  default = "ap-southeast-2"
}

variable "github_repository" {
  type    = string
  default = "logan-han/pollywiki"
}

# All hostnames the site should answer on. First entry is canonical.
# Add pollywiki.au here later; multi-domain is supported from day one.
variable "domains" {
  type    = list(string)
  default = ["pollywiki.han.life"]
}

# Flip to true once the ACM validation CNAMEs (see outputs) are in DNS and the
# certificate is issued. Until then CloudFront serves on its default domain.
variable "enable_custom_domain" {
  type    = bool
  default = false
}

variable "budget_email" {
  type    = string
  default = "lhan@pay.com.au"
}
