# Fork Dessau de RustDesk — con agente de monitoreo integrado

Este repo es un **fork MODIFICADO de RustDesk (AGPL-3.0)** que lleva integrado el
agente de monitoreo de productividad de Dessau. Al distribuirlo a las PCs, la
empresa queda obligada por la AGPL a publicar este código y ofrecerlo a los
usuarios de red (cláusula 13).

## Estado actual (lo que YA está hecho)

- ✅ `src/monitoreo.rs` — el agente: mide actividad (GetLastInputInfo), ventana
  en primer plano (GetForegroundWindow) y reporta por HTTP (reqwest). Sin
  secretos horneados: la config va por entorno (ver `dessau-agente.env.ejemplo`).
- ✅ `src/lib.rs` — declara `pub mod monitoreo` (solo Windows).
- ✅ `src/core_main.rs` — lanza `monitoreo::ejecutar()` en su hilo al arrancar
  el servicio (línea ~206).
- ✅ Submódulo `libs/hbb_common` inicializado.
- ✅ `dessau-agente.env.ejemplo` — plantilla de config del agente.
- ✅ Terreno de build verificado (Rust, vcpkg, libs nativas ya compiladas).

## Lo que FALTA (y solo tú puedes hacer)

### 1. El workflow de CI (crear a mano)

Por seguridad, la creación automática del CI de un build de monitoreo está
bloqueada. Crea manualmente el archivo:

    .github/workflows/dessau-fork-build.yml

con este contenido:

```yaml
name: Dessau Fork Build (Windows)
on:
  workflow_dispatch:
    inputs:
      firmar:
        description: "Firmar el .exe (requiere secrets WINDOWS_PFX_*)"
        type: boolean
        default: false
env:
  RUST_VERSION: "1.75"
  LLVM_VERSION: "15.0.6"
  FLUTTER_VERSION: "3.24.5"
  VCPKG_COMMIT_ID: "9e593bb18ea69cc5095e012465dcd675a822ed0d"
  VCPKG_BINARY_SOURCES: "clear"
jobs:
  build-windows:
    runs-on: windows-2022
    steps:
      - uses: actions/checkout@v4
        with: { submodules: recursive }
      - uses: KyleMayes/install-llvm-action@v2
        with: { version: "15.0.6" }
      - uses: dtolnay/rust-toolchain@stable
        with: { toolchain: "1.75", targets: x86_64-pc-windows-msvc }
      - uses: Swatinem/rust-cache@v2
      - uses: subosito/flutter-action@v2
        with: { flutter-version: "3.24.5", channel: stable }
      - name: vcpkg
        shell: bash
        run: |
          git clone https://github.com/microsoft/vcpkg
          cd vcpkg && git checkout ${{ env.VCPKG_COMMIT_ID }}
          ./bootstrap-vcpkg.sh
          VCPKG=$PWD
          echo "VCPKG_ROOT=$VCPKG" >> $GITHUB_ENV
          cd ..
          "$VCPKG/vcpkg" install --triplet x64-windows-static --x-install-root="$VCPKG/installed"
      - shell: bash
        run: pip3 install --upgrade pip && pip3 install requests
      - name: Build
        shell: bash
        env: { VCPKG_ROOT: "${{ env.VCPKG_ROOT }}" }
        # SIN --hwcodec/--vram: el codec por hardware (Intel Media SDK) no compila en
        # CI (cl.exe error 2 en mfx_decode.cpp) y no lo necesitamos — RustDesk anda con
        # codec por software y el agente de monitoreo no depende del codec.
        run: python3 ./build.py --portable --flutter --skip-portable-pack
      - uses: actions/upload-artifact@v4
        with:
          name: dessau-rustdesk-windows
          path: |
            **/*.exe
            **/*.msi
          if-no-files-found: warn
```

### 2. Subir el fork a un repo propio de GitHub

    git add src/monitoreo.rs src/lib.rs src/core_main.rs dessau-agente.env.ejemplo LEEME-FORK.md
    git commit -m "feat: agente de monitoreo de productividad integrado (fork Dessau)"
    git remote add dessau <URL-de-tu-repo>
    git push dessau <rama>

⚠️ Antes de push público: purgar el historial de secretos si los hubiera y quitar
la marca RustDesk (ver punto 4).

### 3. Compilar

GitHub → Actions → "Dessau Fork Build (Windows)" → Run workflow. El artifact es
el instalador con el agente dentro. (Firma opcional con secrets WINDOWS_PFX_*.)

### 4. Obligaciones AGPL + marca (ANTES de distribuir)

- **Publicar el código fuente** de este fork (obligación AGPL §13), con los
  secretos fuera y el historial limpio.
- **Quitar el nombre y logo "RustDesk"** (su marca es de Purslane Ltd., NO está
  bajo la AGPL) y renombrar el producto.
- Poner un enlace visible **"Código fuente (AGPL-3.0)"** dentro del cliente,
  apuntando a la versión exacta que corre.

### 5. Requisito legal laboral (Ley 29733) — INELUDIBLE

**Informar a los trabajadores del monitoreo ANTES de desplegar o publicar.**
Publicar el diseño de la vigilancia sin este aviso expone a la empresa
públicamente. Este paso va PRIMERO.

## Config del agente en cada PC

Ver `dessau-agente.env.ejemplo`. El agente lee, EN ESTE ORDEN:
1. el archivo `%ProgramData%\Dessau\monitoreo\config` (formato `KEY=VALUE`), que
   el instalador debe escribir con ACL **sólo Administrators+SYSTEM** (icacls,
   quitando `Users`) para que el secreto no quede legible por cualquier usuario;
2. las variables de entorno del proceso (fallback).

Claves: `DESSAU_MONITOREO_URL` (la edge function de Gestión Dessau, la MISMA en
cualquier red), `DESSAU_MONITOREO_SECRET` (= `monitoreo_device_auth.secret` del
proyecto, o la edge da 401), `DESSAU_MONITOREO_APIKEY` (opcional, sólo si la edge
está con `verify_jwt=true`) y `DESSAU_MONITOREO_INTERVALO` (semilla). Sin URL, el
agente queda **inerte**.

## Comportamiento (actualizado 2026-09-01)

- **Sin registro de usuario**: el equipo se identifica por hostname + IP; no hay login.
- **Registra IP + nombre y detecta cambios**: en cada latido reporta la IP LAN
  (elegida hacia el servidor, robusto en PCs multi-homed) y el hostname; el
  BACKEND (trigger sobre `monitoreo_dispositivos`) detecta y audita si cambian.
  El agente además lleva un archivo de estado local y marca `cambio_red`.
- **Autoarranca en el SERVICIO (SYSTEM)**: el agente se lanza en el proceso
  `--service` (core_main.rs), que Windows autoarranca en CADA arranque
  (`sc … start=auto`), **antes del login**, como LocalSystem. Así lee el config
  protegido (ACL Admin+SYSTEM) **sin exponer el secreto a los usuarios** y funciona
  en PCs de usuario estándar. Cumple "corre apenas se prenda la computadora".
- **Fase 1 = REGISTRO**: late con la identidad del equipo (sin actividad) y recibe
  del servidor el intervalo. La MEDICIÓN de actividad (apps/ocio) es **Fase 2**:
  necesita la sesión del usuario (el servicio, en la sesión 0, no ve el escritorio),
  irá en el tray y entregará las muestras al servicio por IPC para que el secreto
  nunca salga del contexto SYSTEM. El indicador/aviso visible de Ley 29733 también
  es Fase 2; hoy la transparencia la da el aviso a los trabajadores fuera de banda.
- **Destino = Gestión Dessau** (edge `monitoreo-actividad-device`), NO Gauzy.

## Recordatorio de arquitectura

El agente NO se comunica con RustDesk: mide el escritorio con Win32 y reporta por
su cuenta. Fusionarlo en RustDesk no le da capacidad nueva — es una decisión de
empaquetado, no técnica. (La alternativa sin AGPL era un instalador único que
pone RustDesk oficial + el agente por separado; ver repo rustdesk-server.)
