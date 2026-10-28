provider "aws" {
  region = var.region

  # Refuse to plan or apply against any other account (wrong credentials, wrong profile).
  allowed_account_ids = [var.aws_account_id]

  # Every taggable resource gets these tags; resources add Name/Component on top.
  default_tags {
    tags = local.tags
  }
}

provider "random" {}

provider "http" {}
