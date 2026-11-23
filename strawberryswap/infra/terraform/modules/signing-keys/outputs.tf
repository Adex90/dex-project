output "key_arns" {
  description = "Signer name => KMS key ARN (the vault's --kms-key-id, the notary's --kms-key-id, forge's AWS_KMS_KEY_ID)."
  value       = { for name, key in aws_kms_key.signer : name => key.arn }
}

output "key_aliases" {
  description = "Signer name => KMS alias (alias/<name>/<signer>)."
  value       = { for name, alias in aws_kms_alias.signer : name => alias.name }
}

output "role_arns" {
  description = "Signer name => the IAM role that alone may sign with that key (each process's AWS profile assumes it)."
  value       = { for name, role in aws_iam_role.signer : name => role.arn }
}
