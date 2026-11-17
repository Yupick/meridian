#!/usr/bin/env bash
# update.sh — Sincronización de Meridian con GitHub (origin).
#
# Modos:
#   --pull         Actualizar el repo local desde origin (fast-forward).
#   --push         Subir la rama actual a origin.
#   --commit       Crear un commit con los cambios locales.
#   -m "texto"     Mensaje del commit (con --commit).
#   -y             No preguntar confirmaciones (uso no interactivo).
#   -h, --help     Mostrar ayuda.
#   Sin argumentos: menú interactivo.
#
# Flujo de GitHub que respeta el script:
#   - No se hacen commits directos en main. Se trabaja en develop o en ramas
#     de funcionalidad (feature/*). main solo se actualiza desde develop por PR.
#   - Nunca se fuerza (no hay force push) ni se reescribe historial.
#   - Si local y remoto han divergido, el script se detiene y avisa.

set -euo pipefail

REMOTE="origin"
PROTECTED_BRANCH="main"

cd "$(dirname "$0")"

log()  { printf '[update] %s\n' "$*"; }
warn() { printf '[update] AVISO: %s\n' "$*" >&2; }
fail() { printf '[update] ERROR: %s\n' "$*" >&2; exit 1; }

usage() { sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; }

ask() {  # ask "pregunta" -> devuelve 0 si sí
    [[ "${ASSUME_YES:-0}" == 1 ]] && return 0
    local resp
    read -r -p "$1 [s/N] " resp
    [[ "$resp" =~ ^[sSyY]$ ]]
}

check_repo() {
    git rev-parse --git-dir >/dev/null 2>&1 || fail "no es un repositorio git"
    git remote get-url "$REMOTE" >/dev/null 2>&1 || fail "no existe el remoto '$REMOTE'"
}

current_branch() { git rev-parse --abbrev-ref HEAD; }

fetch_remote() {
    log "consultando $REMOTE..."
    git fetch --prune --quiet "$REMOTE" || fail "no se pudo conectar con $REMOTE"
}

do_pull() {
    local b; b="$(current_branch)"
    fetch_remote
    if ! git show-ref --verify --quiet "refs/remotes/$REMOTE/$b"; then
        fail "la rama '$b' no existe en $REMOTE"
    fi
    local local_sha remote_sha base_sha
    local_sha="$(git rev-parse HEAD)"
    remote_sha="$(git rev-parse "refs/remotes/$REMOTE/$b")"
    base_sha="$(git merge-base HEAD "refs/remotes/$REMOTE/$b")"

    if [[ "$local_sha" == "$remote_sha" ]]; then
        log "$b: ya está al día con $REMOTE"
    elif [[ "$local_sha" == "$base_sha" ]]; then
        if [[ -n "$(git status --porcelain)" ]]; then
            fail "hay cambios sin confirmar; haz commit o guárdalos (git stash) antes de actualizar"
        fi
        log "$b: remoto adelantado, actualizando (fast-forward)"
        git merge --ff-only -q "$REMOTE/$b" || fail "no se pudo avanzar en modo fast-forward"
    elif [[ "$remote_sha" == "$base_sha" ]]; then
        warn "$b: tienes commits locales que $REMOTE no tiene. Usa --push para subirlos."
    else
        fail "$b: local y remoto han divergido. Resuelve con merge o rebase manualmente"
    fi
}

# Archivos que nunca deben subirse aunque aparezcan en git status
SECRET_PATTERNS=("*.pem" "*.key" "*.p12" "*.env" ".env" "*auth.json" "llA.txt" "*.vars.fd")

do_commit() {
    local b; b="$(current_branch)"
    if [[ "$b" == "$PROTECTED_BRANCH" ]]; then
        fail "no se hacen commits directos en '$PROTECTED_BRANCH'. Cambia a develop o a una rama feature/*"
    fi
    [[ -z "$(git status --porcelain)" ]] && { log "no hay cambios que confirmar"; return 0; }

    log "cambios detectados:"
    git status --short | sed 's/^/  /'

    # Avisar de archivos sensibles antes de añadirlos
    local sensitive=()
    while IFS= read -r f; do
        for p in "${SECRET_PATTERNS[@]}"; do
            # shellcheck disable=SC2254
            case "$(basename "$f")" in $p) sensitive+=("$f");; esac
        done
    done < <(git status --porcelain --untracked-files=all | awk '{print $2}')
    if (( ${#sensitive[@]} > 0 )); then
        warn "archivos sensibles detectados, NO se añadirán:"
        printf '  %s\n' "${sensitive[@]}" >&2
    fi

    if [[ -z "${MESSAGE:-}" ]]; then
        [[ "${ASSUME_YES:-0}" == 1 ]] && fail "falta el mensaje del commit (-m \"texto\")"
        read -r -p "Mensaje del commit: " MESSAGE
    fi
    [[ -n "$MESSAGE" ]] || fail "el mensaje del commit no puede estar vacío"

    ask "¿Añadir todos los cambios y crear el commit en '$b'?" || { log "commit cancelado"; return 0; }

    # Añadir todo excepto los sensibles
    git add -A
    for f in "${sensitive[@]}"; do git reset -q -- "$f" 2>/dev/null || true; done
    # plan.md es parte del repo aunque .opencode tenga reglas de ignorar
    if git status --porcelain -- .opencode/state/plan.md | grep -q .; then
        git add -f .opencode/state/plan.md
    fi

    if git diff --cached --quiet; then
        log "nada nuevo para confirmar tras filtrar archivos sensibles"
        return 0
    fi
    git commit -q -m "$MESSAGE"
    log "commit creado: $(git log --oneline -1)"
}

do_push() {
    local b; b="$(current_branch)"
    if [[ "$b" == "$PROTECTED_BRANCH" ]]; then
        warn "subir directamente a '$PROTECTED_BRANCH'. Lo habitual es hacerlo por PR desde develop."
        ask "¿Continuar?" || { log "push cancelado"; return 0; }
    fi
    fetch_remote
    if git show-ref --verify --quiet "refs/remotes/$REMOTE/$b"; then
        local base remote_sha
        remote_sha="$(git rev-parse "refs/remotes/$REMOTE/$b")"
        base="$(git merge-base HEAD "refs/remotes/$REMOTE/$b")"
        [[ "$remote_sha" == "$base" ]] || fail "$REMOTE/$b tiene cambios que no tienes. Primero ejecuta --pull"
    fi
    if [[ "$(git rev-parse HEAD)" == "$(git rev-parse "refs/remotes/$REMOTE/$b" 2>/dev/null || true)" ]]; then
        log "$b: nada que subir"
        return 0
    fi
    git push -q -u "$REMOTE" "$b"
    log "$b subida a $REMOTE"
}

show_state() {
    log "estado:"
    git branch -vv --list | sed 's/^/  /'
    git status -sb | head -1 | sed 's/^/  /'
}

menu() {
    while true; do
        echo
        echo "=== Meridian: sincronización con GitHub ($(current_branch)) ==="
        echo "  1) Actualizar desde remoto (pull)"
        echo "  2) Subir al remoto (push)"
        echo "  3) Crear commit de cambios locales"
        echo "  4) Commit y subir (3 + 2)"
        echo "  5) Ver estado"
        echo "  6) Salir"
        read -r -p "Opción: " opt
        case "$opt" in
            1) do_pull ;;
            2) do_push ;;
            3) do_commit ;;
            4) do_commit && do_push ;;
            5) show_state ;;
            6|q|"") exit 0 ;;
            *) warn "opción no válida" ;;
        esac
    done
}

# ---- Parseo de argumentos ----
DO_PULL=0; DO_PUSH=0; DO_COMMIT=0; ASSUME_YES=0; MESSAGE=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --pull) DO_PULL=1 ;;
        --push) DO_PUSH=1 ;;
        --commit) DO_COMMIT=1 ;;
        -m) shift; MESSAGE="${1:-}"; [[ -n "$MESSAGE" ]] || fail "-m requiere un texto" ;;
        -y) ASSUME_YES=1 ;;
        -h|--help) usage; exit 0 ;;
        *) fail "opción desconocida: $1 (usa -h)" ;;
    esac
    shift
done

check_repo

if (( DO_PULL + DO_PUSH + DO_COMMIT == 0 )); then
    menu
    exit 0
fi

# Orden: pull -> commit -> push (así el push incluye lo confirmado)
(( DO_PULL ))   && do_pull
(( DO_COMMIT )) && do_commit
(( DO_PUSH ))   && do_push
show_state
