variable "subscription_id" {
  type        = string
  default     = null
  description = "Azure subscription ID. Leave null to use the ARM_SUBSCRIPTION_ID environment variable."

  validation {
    condition     = var.subscription_id == null || can(regex("^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$", var.subscription_id))
    error_message = "subscription_id must be a valid Azure subscription GUID or null."
  }
}

variable "resource_group_name" {
  type        = string
  default     = "zenha-rg"
  description = "Name of the existing Azure Resource Group where resources are provisioned."

  validation {
    condition     = length(trimspace(var.resource_group_name)) > 0
    error_message = "resource_group_name must not be empty."
  }
}

variable "location" {
  type        = string
  default     = "chilecentral"
  description = "Azure region for all resources."

  validation {
    condition     = length(trimspace(var.location)) > 0
    error_message = "location must not be empty."
  }
}

variable "name_prefix" {
  type        = string
  default     = "zenha"
  description = "Base name prefix for resources."

  validation {
    condition     = length(var.name_prefix) >= 2 && length(var.name_prefix) <= 20 && can(regex("^[a-z0-9][a-z0-9-]*[a-z0-9]$", var.name_prefix))
    error_message = "name_prefix must be 2 to 20 lowercase alphanumeric characters or hyphens."
  }
}

variable "vm_size" {
  type        = string
  default     = "Standard_B2ats_v2"
  description = "Virtual machine size."

  validation {
    condition     = length(trimspace(var.vm_size)) > 0
    error_message = "vm_size must not be empty."
  }
}

variable "os_disk_size_gb" {
  type        = number
  default     = 30
  description = "OS managed disk size in GiB (Standard SSD)."

  validation {
    condition     = var.os_disk_size_gb >= 30 && var.os_disk_size_gb <= 64 && floor(var.os_disk_size_gb) == var.os_disk_size_gb
    error_message = "os_disk_size_gb must be an integer between 30 and 64."
  }
}

variable "admin_username" {
  type        = string
  default     = "azureuser"
  description = "Administrator username for the Linux VM."

  validation {
    condition     = length(var.admin_username) >= 1 && length(var.admin_username) <= 64
    error_message = "admin_username must be between 1 and 64 characters."
  }
}

variable "ssh_public_key" {
  type        = string
  description = "SSH public key for the administrator account (e.g. ssh-ed25519 AAAA...)."

  validation {
    condition     = can(regex("^(ssh-ed25519|ssh-rsa|ecdsa-sha2-nistp256|sk-ssh-ed25519@openssh\\.com|sk-ecdsa-sha2-nistp256@openssh\\.com)\\s+", trimspace(var.ssh_public_key)))
    error_message = "ssh_public_key must be a valid OpenSSH public key."
  }
}

variable "dns_label_prefix" {
  type        = string
  description = "Globally unique DNS label for the public IP (<dns_label_prefix>.<location>.cloudapp.azure.com)."

  validation {
    condition     = length(var.dns_label_prefix) >= 3 && length(var.dns_label_prefix) <= 63 && can(regex("^[a-z0-9]([a-z0-9-]*[a-z0-9])?$", var.dns_label_prefix))
    error_message = "dns_label_prefix must be 3 to 63 lowercase alphanumeric characters or hyphens, starting and ending with alphanumeric."
  }
}

variable "ssh_allowed_cidr" {
  type        = string
  default     = ""
  description = "Optional single IPv4 or CIDR allowed to connect to SSH port 22/TCP (e.g. 203.0.113.10/32). Leave empty to keep SSH closed."

  validation {
    condition     = var.ssh_allowed_cidr == "" || can(cidrhost(var.ssh_allowed_cidr, 0))
    error_message = "ssh_allowed_cidr must be empty or a valid CIDR block (for example 203.0.113.10/32)."
  }
}
