---
name: "Db"
mode: "subagent"
description: "DESTINATARIO AUTORIZADO de delegaciones del Orquestador. Diseño BD, migrations, queries, optimización. Validar migraciones (rollback), índices, constraints, no commitear datos sensibles en seeds."
model: "opencode/longcat-2.5-preview-free"
permissions:
  - read
  - bash
  - glob
  - grep
---
 
# Agente Db
