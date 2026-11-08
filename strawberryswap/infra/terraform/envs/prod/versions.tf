terraform {
  required_version = ">= 1.6.0"
  required_providers {
    aws    = { source = "hashicorp/aws", version = "~> 5.80" }
    random = { source = "hashicorp/random", version = "~> 3.6" }
  }

  # Remote state: create the bucket (versioned, encrypted) + lock table once, then uncomment.
  # State contains the generated DB password, so the bucket MUST be private and encrypted.
  # backend "s3" {
  #   bucket         = "strawberryswap-tfstate-<account-id>"
  #   key            = "prod/automator.tfstate"
  #   region         = "us-east-1"
  #   dynamodb_table = "strawberryswap-tflock"
  #   encrypt        = true
  # }
}

provider "aws" {
  region = var.region
  default_tags {
    tags = { Project = "strawberryswap", Env = "prod", ManagedBy = "terraform" }
  }
}
