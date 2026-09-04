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
