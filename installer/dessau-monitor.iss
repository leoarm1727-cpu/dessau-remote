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
; El config del agente (lo escribió el workflow con el secreto). ACL restrictiva abajo.
Source: "_config\config"; DestDir: "{commonappdata}\Dessau\monitoreo"; Flags: ignoreversion

[UninstallRun]
Filename: "{app}\rustdesk.exe"; Parameters: "--uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "DessauForkUninstall"

[UninstallDelete]
Type: filesandordirs; Name: "{commonappdata}\Dessau\monitoreo"

[Code]
procedure BloquearACL();
var
  Dir: string;
  Code: Integer;
begin
  Dir := ExpandConstant('{commonappdata}\Dessau\monitoreo');
  // /inheritance:r quita a Users; solo Administrators (S-1-5-32-544) y SYSTEM
  // (S-1-5-18) con control total. Correcto porque el AGENTE corre como SYSTEM
  // (proceso --service del fork): lee el config sin problema y el secreto queda
  // ILEGIBLE para los usuarios estándar de la PC.
  Exec(ExpandConstant('{sys}\icacls.exe'),
       '"' + Dir + '" /inheritance:r /grant:r "*S-1-5-32-544:(OI)(CI)F" "*S-1-5-18:(OI)(CI)F"',
       '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure InstalarFork();
var
  Code: Integer;
begin
  // --silent-install: copia la app a su ubicación real, crea el servicio (start=auto)
  // y el acceso directo de --tray en Startup. El agente vive en el proceso --service.
  Exec(ExpandConstant('{app}\rustdesk.exe'), '--silent-install',
       '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    // El config ya quedó copiado en ssInstall. Bloquear su ACL ANTES de instalar/
    // arrancar el servicio, para que el primer latido del agente ya lo lea protegido.
    BloquearACL();
    InstalarFork();
  end;
end;
