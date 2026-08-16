terraform {
  required_version = ">= 1.9"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.70"
    }
  }

  backend "s3" {
    bucket = "pollywiki-tfstate-977677890609"
    key    = "infra.tfstate"
    region = "ap-southeast-2"
  }
}

provider "aws" {
  region = var.region
}

# CloudFront certificates must live in us-east-1.
provider "aws" {
  alias  = "us_east_1"
  region = "us-east-1"
}
