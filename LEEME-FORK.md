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
  VCPKG_BINARY_SOURCES: "clear;x-gha,readwrite"
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
          echo "VCPKG_ROOT=$PWD" >> $GITHUB_ENV
      - shell: bash
        run: pip3 install --upgrade pip && pip3 install requests
      - name: Build
        shell: bash
        env: { VCPKG_ROOT: "${{ env.VCPKG_ROOT }}" }
        run: python3 ./build.py --portable --flutter --hwcodec --vram --skip-portable-pack
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

Ver `dessau-agente.env.ejemplo`. Poner `DESSAU_MONITOREO_URL` (según la red),
`DESSAU_MONITOREO_SECRET` e `INTERVALO`. Sin URL, el agente queda inerte.

## Recordatorio de arquitectura

El agente NO se comunica con RustDesk: mide el escritorio con Win32 y reporta por
su cuenta. Fusionarlo en RustDesk no le da capacidad nueva — es una decisión de
empaquetado, no técnica. (La alternativa sin AGPL era un instalador único que
pone RustDesk oficial + el agente por separado; ver repo rustdesk-server.)
