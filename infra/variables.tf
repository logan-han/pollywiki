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

# owner@ownerId/repo@repoId as it appears in this account's customised OIDC
# sub claims (verify with the oidc-debug workflow if it ever changes).
variable "github_owner_id_sub" {
  type    = string
  default = "logan-han@4230053/pollywiki@1333746756"
}

# All hostnames the site should answer on. First entry is canonical.
variable "domains" {
  type    = list(string)
  default = ["pollywiki.au", "www.pollywiki.au"]
}

variable "enable_custom_domain" {
  type    = bool
  default = true
}

# Existing issued certificate covering the domains (pollywiki.au and
# *.pollywiki.au). Set to "" to have Terraform request and validate a new
# certificate instead.
variable "acm_certificate_arn" {
  type    = string
  default = "arn:aws:acm:us-east-1:977677890609:certificate/af0bad36-69df-422c-b860-f552173a8cf8"
}

variable "budget_email" {
  type    = string
  default = "logan@han.life"
}
