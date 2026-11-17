#!/usr/bin/env bash
# update.sh — Sincroniza el repositorio local de Meridian con origin.
#
# Qué hace:
#   1. Comprueba que el árbol de trabajo esté limpio (si no, se detiene).
#   2. Consulta origin (git fetch --prune).
#   3. Para cada rama local (main, develop) con remoto:
#        - al día            -> no hace nada
#        - remoto adelantado -> fast-forward (git pull --ff-only)
#        - local adelantado  -> push
#        - divergidas        -> se detiene y avisa (no fuerza nada)
#   4. Muestra el resultado final.
#
# Uso: ./update.sh [rama ...]     (por defecto: main develop)

set -euo pipefail

REMOTE="origin"
BRANCHES=("$@")
[[ ${#BRANCHES[@]} -eq 0 ]] && BRANCHES=(main develop)

cd "$(dirname "$0")"

log()  { printf '[update] %s\n' "$*"; }
fail() { printf '[update] ERROR: %s\n' "$*" >&2; exit 1; }

git rev-parse --git-dir >/dev/null 2>&1 || fail "no es un repositorio git"
git remote get-url "$REMOTE" >/dev/null 2>&1 || fail "no existe el remoto '$REMOTE'"

# 1. Árbol limpio
if [[ -n "$(git status --porcelain)" ]]; then
    fail "hay cambios sin confirmar; confírmalos o guárdalos (git stash) antes de sincronizar"
fi

ORIGINAL_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
restore_branch() { git checkout -q "$ORIGINAL_BRANCH" 2>/dev/null || true; }
trap restore_branch EXIT

# 2. Consultar remoto
log "consultando $REMOTE..."
git fetch --prune --quiet "$REMOTE" || fail "no se pudo conectar con $REMOTE"

# 3. Sincronizar cada rama
for b in "${BRANCHES[@]}"; do
    if ! git show-ref --verify --quiet "refs/heads/$b"; then
        log "$b: no existe localmente, se omite"
        continue
    fi
    if ! git show-ref --verify --quiet "refs/remotes/$REMOTE/$b"; then
        log "$b: no existe en $REMOTE; haz push con: git push -u $REMOTE $b"
        continue
    fi

    local_sha="$(git rev-parse "refs/heads/$b")"
    remote_sha="$(git rev-parse "refs/remotes/$REMOTE/$b")"
    base_sha="$(git merge-base "refs/heads/$b" "refs/remotes/$REMOTE/$b")"

    if [[ "$local_sha" == "$remote_sha" ]]; then
        log "$b: al día"
    elif [[ "$local_sha" == "$base_sha" ]]; then
        log "$b: remoto adelantado, actualizando (fast-forward)"
        git checkout -q "$b"
        git merge --ff-only -q "$REMOTE/$b" || fail "$b no se pudo avanzar en modo fast-forward"
    elif [[ "$remote_sha" == "$base_sha" ]]; then
        log "$b: local adelantado, subiendo cambios"
        git push -q "$REMOTE" "$b"
    else
        fail "$b: local y remoto han divergido. Resuelve con merge o rebase manualmente; este script no fuerza cambios"
    fi
done

# 4. Estado final
log "estado final:"
git branch -vv --list "${BRANCHES[@]}" | sed 's/^/  /'
log "sincronización completa"
