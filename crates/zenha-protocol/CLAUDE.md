# zenha-protocol

Tipos compartilhados entre `zenha-client` e `zenha-server`.

- Só `serde`. Sem I/O, sem async, sem dependências pesadas.
- Mudar um campo quebra cliente e servidor ao mesmo tempo: atualize os dois no mesmo commit.
- Todo tipo novo ganha um teste de ida e volta em JSON.
- Limites de tamanho (`MAX_*`) ficam aqui e são validados nas duas pontas.
