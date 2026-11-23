output "instance_ids" { value = aws_instance.bot[*].id }
output "role_name" { value = aws_iam_role.bot.name }
output "role_arn" { value = aws_iam_role.bot.arn }
