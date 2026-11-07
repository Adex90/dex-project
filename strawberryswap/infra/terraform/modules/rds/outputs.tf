output "address" { value = aws_db_instance.this.address }
output "port" { value = aws_db_instance.this.port }
output "db_name" { value = aws_db_instance.this.db_name }
output "username" { value = aws_db_instance.this.username }
output "password_parameter_name" { value = aws_ssm_parameter.db_password.name }
output "password_parameter_arn" { value = aws_ssm_parameter.db_password.arn }
