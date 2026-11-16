locals {
  schedule_map = var.schedule_enabled ? { for item in var.schedules : item.name => item } : {}
}

resource "azurerm_logic_app_workflow" "schedule" {
  for_each = local.schedule_map

  name                = "${var.name_prefix}-schedule-${each.key}"
  location            = var.location
  resource_group_name = data.azurerm_resource_group.main.name
  enabled             = true

  identity {
    type = "SystemAssigned"
  }
}

resource "azurerm_logic_app_trigger_recurrence" "schedule" {
  for_each = local.schedule_map

  name         = "Recurrence"
  logic_app_id = azurerm_logic_app_workflow.schedule[each.key].id
  frequency    = "Week"
  interval     = 1
  time_zone    = var.schedule_time_zone

  schedule {
    on_these_days    = each.value.days
    at_these_hours   = [each.value.hour]
    at_these_minutes = [each.value.minute]
  }
}

resource "azurerm_logic_app_action_custom" "schedule" {
  for_each = local.schedule_map

  name         = "VmPowerAction"
  logic_app_id = azurerm_logic_app_workflow.schedule[each.key].id

  body = jsonencode({
    type             = "Http"
    runAfter         = {}
    operationOptions = "DisableAsyncPattern"
    inputs = {
      method = "POST"
      uri    = "https://management.azure.com${azurerm_linux_virtual_machine.main.id}/${each.value.action}?api-version=2024-07-01"
      authentication = {
        type     = "ManagedServiceIdentity"
        audience = "https://management.azure.com/"
      }
    }
  })

  depends_on = [azurerm_logic_app_trigger_recurrence.schedule]
}

resource "azurerm_role_assignment" "schedule_vm_contributor" {
  for_each = local.schedule_map

  scope                            = azurerm_linux_virtual_machine.main.id
  role_definition_name             = "Virtual Machine Contributor"
  principal_id                     = azurerm_logic_app_workflow.schedule[each.key].identity[0].principal_id
  principal_type                   = "ServicePrincipal"
  skip_service_principal_aad_check = true

  # Azure returns the scope with "resourcegroups" in lower case and does not return skip_service_principal_aad_check.
  lifecycle {
    ignore_changes = [scope, skip_service_principal_aad_check]
  }
}
