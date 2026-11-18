# `terraform test` (no AWS account needed): plans the module with placeholder credentials, the account id and the
# signer roles' ARNs overridden, and checks the policies Terraform renders: who may sign, with which key, and how.

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
  name = "strawberryswap"
  signers = {
    operator  = ["arn:aws:iam::123456789012:role/strawberryswap-bot-host"]
    relayer   = ["arn:aws:iam::123456789012:role/strawberryswap-bot-host"]
    automator = ["arn:aws:iam::123456789012:role/strawberryswap-bot-host"]
    notary    = ["arn:aws:iam::123456789012:role/strawberryswap-bot-host"]
    deployer  = ["arn:aws:iam::123456789012:role/strawberryswap-deployers"]
  }
}

run "one_secp256k1_signing_key_and_one_role_per_signer" {
  command = plan

  assert {
    condition     = toset(keys(aws_kms_key.signer)) == toset(["operator", "relayer", "automator", "notary", "deployer"])
    error_message = "One key per signer."
  }
  assert {
    condition     = alltrue([for k in aws_kms_key.signer : k.customer_master_key_spec == "ECC_SECG_P256K1" && k.key_usage == "SIGN_VERIFY" && !k.enable_key_rotation])
    error_message = "Every key is a non-rotating secp256k1 signing key (a rotated key would be a new address)."
  }
  assert {
    condition     = aws_kms_alias.signer["operator"].name == "alias/strawberryswap/operator"
    error_message = "Aliases are alias/<name>/<signer>."
  }
  assert {
    condition     = aws_iam_role.signer["notary"].name == "strawberryswap-signer-notary"
    error_message = "Roles are <name>-signer-<signer>."
  }
}

run "only_the_signers_role_signs_and_only_digests" {
  command = plan

  # The signer's statements name its role alone; Sign is limited to ECDSA over a 32-byte digest.
  assert {
    condition = alltrue([
      for name, doc in data.aws_iam_policy_document.key : alltrue([
        for s in jsondecode(doc.json).Statement : (
          contains(flatten([s.Action]), "kms:Sign")
          ? (
            s.Principal.AWS == "arn:aws:iam::123456789012:role/strawberryswap-signer-${name}"
            && s.Condition.StringEquals["kms:SigningAlgorithm"] == "ECDSA_SHA_256"
            && s.Condition.StringEquals["kms:MessageType"] == "DIGEST"
          )
          : true
        )
      ])
    ])
    error_message = "kms:Sign must go to the key's own signer role only, for ECDSA_SHA_256 over a DIGEST."
  }
  assert {
    condition = alltrue([
      for name, doc in data.aws_iam_policy_document.key : length([
        for s in jsondecode(doc.json).Statement : s if contains(flatten([s.Action]), "kms:Sign")
      ]) == 1
    ])
    error_message = "Exactly one statement per key grants kms:Sign."
  }
  assert {
    condition     = aws_iam_role.signer["relayer"].name == "strawberryswap-signer-relayer"
    error_message = "The role the relayer key's policy names is the relayer's role."
  }
  # The account (its administrators) manages the key but holds no signing action, no grant and no wildcard.
  assert {
    condition = alltrue([
      for name, doc in data.aws_iam_policy_document.key : alltrue([
        for s in jsondecode(doc.json).Statement : (
          s.Principal.AWS == "arn:aws:iam::123456789012:root"
          ? length(setintersection(toset(flatten([s.Action])), toset(["kms:Sign", "kms:Verify", "kms:CreateGrant", "kms:*", "kms:Create*"]))) == 0
          : true
        )
      ])
    ])
    error_message = "The account root must never be granted kms:Sign, kms:Verify, kms:CreateGrant or a wildcard."
  }
  assert {
    condition = alltrue([
      for name, doc in data.aws_iam_policy_document.key : length(jsondecode(doc.json).Statement) == 3
    ])
    error_message = "Three statements per key: administration, sign, read the public key."
  }
}

run "each_role_is_assumable_by_its_own_principals_only" {
  command = plan

  assert {
    condition     = jsondecode(data.aws_iam_policy_document.assume["deployer"].json).Statement[0].Principal.AWS == "arn:aws:iam::123456789012:role/strawberryswap-deployers"
    error_message = "The deployer's role is assumable by the deployers only."
  }
  assert {
    condition     = jsondecode(data.aws_iam_policy_document.assume["operator"].json).Statement[0].Principal.AWS == "arn:aws:iam::123456789012:role/strawberryswap-bot-host"
    error_message = "The operator's role is assumable by the bot host only."
  }
  assert {
    condition     = jsondecode(data.aws_iam_policy_document.assume["operator"].json).Statement[0].Action == "sts:AssumeRole"
    error_message = "Only sts:AssumeRole."
  }
}

run "a_root_principal_is_refused" {
  command = plan
  variables {
    signers = { operator = ["arn:aws:iam::123456789012:root"] }
  }
  expect_failures = [var.signers]
}

run "a_signer_nobody_can_assume_is_refused" {
  command = plan
  variables {
    signers = { operator = [] }
  }
  expect_failures = [var.signers]
}

run "a_short_deletion_window_is_refused" {
  command = plan
  variables {
    deletion_window_in_days = 3
  }
  expect_failures = [var.deletion_window_in_days]
}
