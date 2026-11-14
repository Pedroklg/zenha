using './main.bicep'

param location = 'chilecentral'
param namePrefix = 'zenha'
param vmSize = 'Standard_B2ats_v2'
param osDiskSizeGB = 30
param adminUsername = 'azureuser'
param sshPublicKey = 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIReplaceWithYourOwnPublicKey comment'
param dnsLabelPrefix = 'zenha-voice-example'
param sshAllowedCidr = ''
