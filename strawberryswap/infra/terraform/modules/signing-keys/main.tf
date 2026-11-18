# The testnet's signing keys: one secp256k1 key in AWS KMS per signer (the vaults' operator, relayer and automator,
# the notary and optionally the deployer), each usable by exactly one IAM role, and by that role only to sign 32-byte
# digests (a transaction's signing hash, or the notary's attestation digest).
#
# * The keys are generated inside KMS's HSMs and cannot be exported. Nothing in Terraform state is secret: state holds
#   key ids, ARNs and policies, never key material.
# * The key policy is the only grant (no IAM policy can widen it): the account's administrators manage the key but
#   cannot sign with it; the signer's role signs digests and reads the public key. Every Sign is a CloudTrail
#   management event, besides the vault's own audit line.
# * Each signer's role is assumable only by the principals named for it (the bot host's instance role for the vaults
#   and the notary; people or a CI role for the deployer).
# * About $1 a month per key, plus $0.15 per 10,000 signatures (asymmetric requests are outside the KMS free tier).
data "aws_caller_identity" "current" {}
data "aws_partition" "current" {}

locals {
  account_arn  = "arn:${data.aws_partition.current.partition}:iam::${data.aws_caller_identity.current.account_id}"
  account_root = "${local.account_arn}:root"
  # Each signer role's ARN from its fixed name, so the key policies render at plan time (and in `terraform test`).
  role_names = { for signer, _ in var.signers : signer => "${var.name}-signer-${signer}" }
  role_arns  = { for signer, role in local.role_names : signer => "${local.account_arn}:role/${role}" }
}

data "aws_iam_policy_document" "assume" {
  for_each = var.signers

  statement {
    sid     = "AssumedByItsPrincipalsOnly"
    actions = ["sts:AssumeRole"]
    principals {
      type        = "AWS"
      identifiers = each.value
    }
  }
}

resource "aws_iam_role" "signer" {
  for_each = var.signers

  name                 = local.role_names[each.key]
  description          = "${var.name}: signs with the ${each.key} KMS key (digests only), nothing else"
  assume_role_policy   = data.aws_iam_policy_document.assume[each.key].json
  max_session_duration = 3600
  tags                 = { Signer = each.key }
}

data "aws_iam_policy_document" "key" {
  for_each = var.signers

  # Administration through IAM in this account, without kms:Sign, kms:Verify or kms:CreateGrant: an administrator can
  # disable the key or schedule its deletion, but cannot sign with it or hand signing to someone else short of
  # rewriting this policy (kms:PutKeyPolicy, itself a CloudTrail event).
  statement {
    sid = "AccountAdministersButNeverSigns"
    actions = [
      "kms:CancelKeyDeletion",
      "kms:CreateAlias",
      "kms:DeleteAlias",
      "kms:DescribeKey",
      "kms:DisableKey",
      "kms:EnableKey",
      "kms:GetKeyPolicy",
      "kms:GetKeyRotationStatus",
      "kms:GetPublicKey",
      "kms:ListGrants",
      "kms:ListKeyPolicies",
      "kms:ListResourceTags",
      "kms:PutKeyPolicy",
      "kms:RevokeGrant",
      "kms:ScheduleKeyDeletion",
      "kms:TagResource",
      "kms:UntagResource",
      "kms:UpdateAlias",
      "kms:UpdateKeyDescription",
    ]
    resources = ["*"] # in a key policy, "*" is this key
    principals {
      type        = "AWS"
      identifiers = [local.account_root]
    }
  }

  statement {
    sid       = "SignerSignsDigestsOnly"
    actions   = ["kms:Sign"]
    resources = ["*"]
    principals {
      type        = "AWS"
      identifiers = [local.role_arns[each.key]]
    }
    condition {
      test     = "StringEquals"
      variable = "kms:SigningAlgorithm"
      values   = ["ECDSA_SHA_256"]
    }
    condition {
      test     = "StringEquals"
      variable = "kms:MessageType"
      values   = ["DIGEST"]
    }
  }

  statement {
    sid       = "SignerReadsThePublicKey"
    actions   = ["kms:GetPublicKey", "kms:DescribeKey"]
    resources = ["*"]
    principals {
      type        = "AWS"
      identifiers = [local.role_arns[each.key]]
    }
  }
}

# Asymmetric KMS keys cannot rotate: a rotated key would be a new Ethereum address, so rotation is a redeployment
# (or, for the operator, SearcherExecutor's new operator) by design, not a KMS setting.
#trivy:ignore:AWS-0065 Asymmetric (ECC_SECG_P256K1, SIGN_VERIFY) keys do not support automatic rotation; a new key is a new address.
resource "aws_kms_key" "signer" {
  for_each = var.signers

  description              = "${var.name}: the ${each.key}'s secp256k1 signing key (testnet; digests only)"
  customer_master_key_spec = "ECC_SECG_P256K1"
  key_usage                = "SIGN_VERIFY"
  enable_key_rotation      = false
  deletion_window_in_days  = var.deletion_window_in_days
  policy                   = data.aws_iam_policy_document.key[each.key].json
  tags                     = { Signer = each.key }

  # KMS refuses a key policy whose principal does not exist yet.
  depends_on = [aws_iam_role.signer]
}

resource "aws_kms_alias" "signer" {
  for_each = var.signers

  name          = "alias/${var.name}/${each.key}"
  target_key_id = aws_kms_key.signer[each.key].key_id
}
