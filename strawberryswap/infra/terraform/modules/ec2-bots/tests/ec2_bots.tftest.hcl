# `terraform test` (no AWS account needed): plans the module with placeholder credentials and the account id
# overridden, and checks that the host runs the image pinned in ami_id. A new image replaces the host and deletes
# its disk, so it must come only from a deliberate change of the pin, never from the floating "latest" parameter.

provider "aws" {
  region                      = "us-east-1"
  access_key                  = "placeholder"
  secret_key                  = "placeholder"
  skip_credentials_validation = true
  skip_requesting_account_id  = true
  skip_metadata_api_check     = true
}

override_data {
  target          = data.aws_caller_identity.current
  override_during = plan
  values = {
    account_id = "123456789012"
    arn        = "arn:aws:iam::123456789012:user/terraform"
    user_id    = "AIDAPLACEHOLDER"
  }
}

variables {
  name                       = "strawberryswap"
  ami_id                     = "ami-0123456789abcdef0"
  public_subnet_ids          = ["subnet-0123456789abcdef0"]
  security_group_id          = "sg-0123456789abcdef0"
  db_host                    = "db.strawberryswap.internal"
  db_port                    = 5432
  db_name                    = "strawberryswap"
  db_user                    = "strawberryswap"
  db_password_parameter_name = "/strawberryswap/db/password"
  metadata_bucket_name       = "strawberryswap-metadata"
  metadata_bucket_arn        = "arn:aws:s3:::strawberryswap-metadata"
  metadata_kms_key_arn       = "arn:aws:kms:us-east-1:123456789012:key/00000000-0000-0000-0000-000000000000"
  signer_role_arns = {
    operator  = "arn:aws:iam::123456789012:role/strawberryswap-signer-operator"
    relayer   = "arn:aws:iam::123456789012:role/strawberryswap-signer-relayer"
    automator = "arn:aws:iam::123456789012:role/strawberryswap-signer-automator"
    notary    = "arn:aws:iam::123456789012:role/strawberryswap-signer-notary"
  }
}

run "the_host_runs_the_pinned_image" {
  command = plan

  assert {
    condition     = length(aws_instance.bot) == 1 && aws_instance.bot[0].ami == "ami-0123456789abcdef0"
    error_message = "The bot host must run exactly the image pinned in ami_id."
  }
}

run "a_short_ami_id_is_a_pin_too" {
  command = plan

  variables {
    ami_id = "ami-0a1b2c3d"
  }

  assert {
    condition     = aws_instance.bot[0].ami == "ami-0a1b2c3d"
    error_message = "An 8-digit AMI id is a valid pin."
  }
}

run "refuses_the_floating_latest_parameter" {
  command = plan

  variables {
    ami_id = "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64"
  }

  expect_failures = [var.ami_id]
}

run "refuses_an_image_name" {
  command = plan

  variables {
    ami_id = "al2023-ami-2023.9.20261110.0-kernel-6.1-x86_64"
  }

  expect_failures = [var.ami_id]
}
