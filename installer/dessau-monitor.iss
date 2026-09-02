; =============================================================================
;  Dessau Soporte y Productividad — INSTALADOR ÚNICO (Inno Setup)
; -----------------------------------------------------------------------------
;  Produce UN solo "Dessau-Setup.exe" que:
;    1. empaqueta la app Flutter compilada (rustdesk.exe + DLLs + data/) y corre
;       `rustdesk.exe --silent-install` (instala el fork como servicio con
;       autoarranque; el agente de monitoreo corre en el proceso --service, SYSTEM);
;    2. escribe la config del agente (URL + secreto) en
;       %ProgramData%\Dessau\monitoreo\config con ACL SOLO Administrators+SYSTEM;
;    3. NO instala Gauzy y NO pide registro de usuario (desatendido).
;
;  El equipo se identifica por hostname + IP; el backend detecta si cambian.
;
;  Se compila DENTRO del workflow del fork (dessau-fork-build.yml), reusando los
;  secrets CERT_PFX_BASE64 / CERT_PFX_PASSWORD (firma) y DESSAU_MONITOREO_SECRET.
;  Todo se inyecta por ENTORNO al compilar (NO queda en el repo):
;      DESSAU_APP_DIR    = carpeta Release del build (rustdesk.exe + DLLs + data)
;      DESSAU_MONITOREO_URL / DESSAU_MONITOREO_SECRET / DESSAU_MONITOREO_APIKEY
;      ISCC.exe /DFirmar "/Sdessau=<cmd signtool>" installer\dessau-monitor.iss
;  Sin /DFirmar compila sin firmar (prueba local).
;
;  ⚠️ Antes de distribuir: avisar a los trabajadores (Ley 29733) y publicar el
;     código del fork (AGPL). Ver LEEME-FORK.md.
; =============================================================================

#define AppNombre "Dessau Soporte y Productividad"
#define AppVersion "1.0.0"
#define AppPublisher "Dessau S&Z S.A."

; --- Inyectados por entorno al compilar (con defaults sensatos) ---------------
#ifndef Url
  #define Url GetEnv("DESSAU_MONITOREO_URL")
#endif
#if Url == ""
  #define Url "https://pwjlxoladntvlnzhnqyi.supabase.co/functions/v1/monitoreo-actividad-device"
#endif

#ifndef Secret
  #define Secret GetEnv("DESSAU_MONITOREO_SECRET")
#endif
#if Secret == ""
  #error Falta DESSAU_MONITOREO_SECRET: definilo por entorno antes de compilar (= monitoreo_device_auth.secret).
#endif

#ifndef ApiKey
  #define ApiKey GetEnv("DESSAU_MONITOREO_APIKEY")
#endif

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
ArchitecturesInstall64Bit=x64compatible
ArchitecturesAllowed=x64compatible
WizardStyle=modern
; --- Firma Authenticode (solo cuando se compila con /DFirmar en CI) -----------
#ifdef Firmar
SignTool=dessau
SignedUninstaller=yes
#endif

[Languages]
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"

[Files]
; La app Flutter completa (rustdesk.exe + DLLs + data/). El fork se auto-instala con
; --silent-install (abajo), que copia todo a su ubicación real y crea el servicio.
Source: "{#AppDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Dirs]
Name: "{commonappdata}\Dessau\monitoreo"

[UninstallRun]
Filename: "{app}\rustdesk.exe"; Parameters: "--uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "DessauForkUninstall"

[UninstallDelete]
Type: filesandordirs; Name: "{commonappdata}\Dessau\monitoreo"

[Code]
procedure EscribirConfig();
var
  Dir, Archivo, Contenido: string;
begin
  Dir := ExpandConstant('{commonappdata}\Dessau\monitoreo');
  ForceDirectories(Dir);
  Archivo := Dir + '\config';
  Contenido :=
    'DESSAU_MONITOREO_URL={#Url}'      + #13#10 +
    'DESSAU_MONITOREO_SECRET={#Secret}' + #13#10 +
    'DESSAU_MONITOREO_APIKEY={#ApiKey}' + #13#10 +
    'DESSAU_MONITOREO_INTERVALO=60'     + #13#10;
  SaveStringToFile(Archivo, Contenido, False);
end;

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
    // Orden: 1) config + ACL ANTES de instalar/arrancar el servicio, para que el
    // primer latido del agente ya tenga endpoint y secreto; 2) instalar el fork.
    EscribirConfig();
    BloquearACL();
    InstalarFork();
  end;
end;
