; =============================================================================
;  Dessau Soporte y Productividad — INSTALADOR ÚNICO (Inno Setup)
; -----------------------------------------------------------------------------
;  Produce UN solo "Dessau-Setup.exe" que:
;    1. empaqueta la app Flutter compilada (rustdesk.exe + DLLs + data/) y corre
;       `rustdesk.exe --silent-install` (instala el fork como servicio con
;       autoarranque; el agente de monitoreo corre en el proceso --service, SYSTEM);
;    2. coloca la config del agente en %ProgramData%\Dessau\monitoreo\config con
;       ACL SOLO Administrators+SYSTEM;
;    3. NO instala Gauzy y NO pide registro de usuario (desatendido).
;
;  El equipo se identifica por hostname + IP; el backend detecta si cambian.
;
;  Se compila DENTRO del workflow del fork (dessau-fork-build.yml). El archivo de
;  config (con el secreto) lo ESCRIBE el workflow en `installer\_config\config`
;  (pwsh, robusto a cualquier valor) y este .iss solo lo empaqueta — el secreto NO
;  pasa por el preprocesador ni por Pascal. La firma es opcional (/DFirmar en CI).
;  DESSAU_APP_DIR = carpeta Release del build. Todo se inyecta por entorno.
;
;  ⚠️ Antes de distribuir: avisar a los trabajadores (Ley 29733) y publicar el
;     código del fork (AGPL). Ver LEEME-FORK.md.
; =============================================================================

#define AppNombre "Dessau Soporte y Productividad"
#define AppVersion "1.0.0"
#define AppPublisher "Dessau S&Z S.A."

; Carpeta con la app Flutter YA compilada: rustdesk.exe + DLLs + data/.
#ifndef AppDir
  #define AppDir GetEnv("DESSAU_APP_DIR")
#endif
#if AppDir == ""
  #define AppDir "flutter\build\windows\x64\runner\Release"
#endif

[Setup]
AppId={{7B2D9E14-3C6A-4F58-9E1D-DE55A0A11C01}
AppName={#AppNombre}
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={autopf}\Dessau\Monitor
DefaultGroupName=Dessau
DisableProgramGroupPage=yes
UninstallDisplayName={#AppNombre}
OutputDir=Output
OutputBaseFilename=Dessau-Setup
Compression=lzma2/max
SolidCompression=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
WizardStyle=modern
#ifdef Firmar
SignTool=dessau
SignedUninstaller=yes
#endif

[Languages]
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"

[Dirs]
Name: "{commonappdata}\Dessau\monitoreo"

[Files]
; La app Flutter completa (rustdesk.exe + DLLs + data/). El fork se auto-instala con
; --silent-install (abajo), que copia todo a su ubicación real y crea el servicio.
Source: "{#AppDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
; El config del agente: SOLO la URL (sin secreto — cada equipo genera el suyo en
; runtime). Legible por el usuario (el agente corre en --tray); no hay nada sensible.
Source: "_config\config"; DestDir: "{commonappdata}\Dessau\monitoreo"; Flags: ignoreversion

[UninstallRun]
Filename: "{app}\rustdesk.exe"; Parameters: "--uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "DessauForkUninstall"

[UninstallDelete]
Type: filesandordirs; Name: "{commonappdata}\Dessau\monitoreo"

[Code]
// ── Limpieza de instalación anterior (evita incompatibilidades) ─────────────
// Pedido: "que instalar siempre desinstale y limpie lo del anterior instalador".
// Reinstalar SOBRE una versión previa sin desinstalarla primero es exactamente
// lo que causó el bug del ACL bloqueado (2026-09-04): un candado de una versión
// vieja quedaba puesto sobre el config nuevo porque Inno no resetea permisos de
// una carpeta que ya existía. Ahora, ANTES de copiar un solo archivo, se corre
// el desinstalador de la versión anterior (si existe) — que a su vez ejecuta SU
// PROPIO [UninstallRun] (rustdesk.exe --uninstall: para el servicio, borra el
// acceso directo de Startup) y su [UninstallDelete] (borra %ProgramData%\Dessau\
// monitoreo completo, incluida cualquier config/ACL vieja) — así cada instalación
// arranca de cero, sin importar qué versión hubiera antes.
function ObtenerRutaDesinstaladorAnterior(): String;
var
  Cmd: String;
begin
  Result := '';
  // {#SetupSetting("AppId")} expande al GUID de [Setup] (sin duplicar la llave);
  // es la MISMA clave donde Inno registra el desinstalador de cualquier versión
  // anterior de este fork (mismo AppId = mismo producto, no toca otros programas).
  if RegQueryStringValue(HKLM,
       'Software\Microsoft\Windows\CurrentVersion\Uninstall\{#SetupSetting("AppId")}_is1',
       'UninstallString', Cmd) then
  begin
    Cmd := Trim(Cmd);
    // El valor viene entre comillas: "C:\...\unins000.exe". Se las quitamos.
    if (Length(Cmd) >= 2) and (Cmd[1] = '"') then
    begin
      Delete(Cmd, 1, 1);
      if Pos('"', Cmd) > 0 then
        Cmd := Copy(Cmd, 1, Pos('"', Cmd) - 1);
    end;
    Result := Cmd;
  end;
end;

procedure LimpiarInstalacionAnterior();
var
  RutaDesinstalador: String;
  ResultCode: Integer;
begin
  RutaDesinstalador := ObtenerRutaDesinstaladorAnterior();
  if (RutaDesinstalador <> '') and FileExists(RutaDesinstalador) then
  begin
    Exec(RutaDesinstalador, '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART', '',
         SW_HIDE, ewWaitUntilTerminated, ResultCode);
  end;
  // Red de seguridad ADICIONAL, best-effort (se ignoran errores de cada Exec):
  // por si el desinstalador de arriba no existía (copia manual del instalador
  // sin pasar por Windows, registro corrupto) o dejó algo a medias. Nunca
  // bloquea la instalación si alguno de estos pasos falla.
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM rustdesk.exe /T', '',
       SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\sc.exe'), 'stop RustDesk', '',
       SW_HIDE, ewWaitUntilTerminated, ResultCode);
  Exec(ExpandConstant('{sys}\sc.exe'), 'delete RustDesk', '',
       SW_HIDE, ewWaitUntilTerminated, ResultCode);
end;

function InitializeSetup(): Boolean;
begin
  LimpiarInstalacionAnterior();
  Result := True;
end;

procedure InstalarFork();
var
  Code: Integer;
begin
  // --silent-install: copia la app a su ubicación real, crea el servicio (start=auto)
  // y el acceso directo de --tray en Startup. El agente de monitoreo corre en el
  // proceso --tray (sesión del usuario), donde ve el escritorio y muestra el indicador.
  Exec(ExpandConstant('{app}\rustdesk.exe'), '--silent-install',
       '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  ResultCode: Integer;
begin
  if CurStep = ssPostInstall then
  begin
    // El config (solo la URL) ya quedó copiado en ssInstall, legible por el usuario.
    // No se bloquea con ACL: no hay secreto que proteger (es por-dispositivo, runtime).
    //
    // 🔑 icacls /reset SIEMPRE, aunque la carpeta sea nueva: una versión ANTERIOR de
    // este instalador SÍ bloqueaba el ACL (protegía un secreto compartido). Si la
    // carpeta ya existía de esa versión vieja, Inno NO le toca los permisos al
    // sobrescribir el archivo -> el candado viejo queda puesto sobre el config nuevo
    // y el agente (corre como usuario normal en --tray) no puede leer la URL y queda
    // inerte en silencio. Verificado en PC726 (2026-09-04): "sin DESSAU_MONITOREO_URL
    // — agente inerte" hasta resetear el ACL a mano.
    Exec(ExpandConstant('{sys}\icacls.exe'),
         ExpandConstant('"{commonappdata}\Dessau\monitoreo" /reset /T'),
         '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    InstalarFork();
  end;
end;
