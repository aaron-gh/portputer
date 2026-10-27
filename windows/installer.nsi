; Portputer's Windows installer, built on the Mac with NSIS by windows/build.sh:
;   makensis -DVERSION=0.1.0 -DVERSION_NUMBERS=0.1.0 -DBUILD=40 -DSTAGE=<folder of files> -DOUTFILE=<setup.exe> windows/installer.nsi
;
; It installs for the current user, in their own Programs folder, so it needs
; no administrator rights, and adds Portputer to the Start menu and to
; Installed apps. Updates run it with /S: it installs quietly and starts
; Portputer again. Pairings and settings are in Portputer's data folder,
; never touched here.

Unicode true
SetCompressor /SOLID lzma
RequestExecutionLevel user
ManifestDPIAware true

!define NAME "Portputer"
!define EXE "Portputer.exe"
!define UNINSTALL_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Portputer"
!define RUN_KEY "Software\Microsoft\Windows\CurrentVersion\Run"

Name "${NAME}"
OutFile "${OUTFILE}"
InstallDir "$LOCALAPPDATA\Programs\Portputer"
InstallDirRegKey HKCU "${UNINSTALL_KEY}" "InstallLocation"
BrandingText "Portputer ${VERSION}"

VIProductVersion "${VERSION_NUMBERS}.${BUILD}"
VIAddVersionKey "ProductName" "${NAME}"
VIAddVersionKey "ProductVersion" "${VERSION}"
VIAddVersionKey "FileDescription" "${NAME} installer"
VIAddVersionKey "FileVersion" "${VERSION}"
VIAddVersionKey "LegalCopyright" "Apache License 2.0"

!include "MUI2.nsh"

!define MUI_ABORTWARNING
!define MUI_FINISHPAGE_RUN "$INSTDIR\${EXE}"
!define MUI_FINISHPAGE_RUN_TEXT "Start ${NAME}"
!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; Portputer has to quit before its files can be replaced. An update quits it
; itself; otherwise, the user is asked to.
!macro WaitForPortputerToQuit
    StrCpy $1 0
    loop:
        FindWindow $0 "PortputerWindow"
        IntCmp $0 0 done
        IfSilent 0 ask
            ; An update quits Portputer as the installer starts; give it a moment.
            IntOp $1 $1 + 1
            IntCmp $1 30 gave_up 0 gave_up
            Sleep 500
            Goto loop
        ask:
            MessageBox MB_RETRYCANCEL|MB_ICONEXCLAMATION "${NAME} is running. Quit it from its notification area icon, then choose Retry." IDRETRY loop
            Abort
        gave_up:
            Abort
    done:
!macroend

Section "Install"
    !insertmacro WaitForPortputerToQuit
    SetOutPath "$INSTDIR"
    File /r "${STAGE}/*"
    WriteUninstaller "$INSTDIR\Uninstall.exe"

    CreateShortcut "$SMPROGRAMS\${NAME}.lnk" "$INSTDIR\${EXE}"

    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayName" "${NAME}"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayVersion" "${VERSION}"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "Publisher" "aaron-gh"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "DisplayIcon" "$INSTDIR\${EXE}"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
    WriteRegStr HKCU "${UNINSTALL_KEY}" "UninstallString" '"$INSTDIR\Uninstall.exe"'
    WriteRegStr HKCU "${UNINSTALL_KEY}" "QuietUninstallString" '"$INSTDIR\Uninstall.exe" /S'
    WriteRegStr HKCU "${UNINSTALL_KEY}" "URLInfoAbout" "https://github.com/aaron-gh/portputer"
    WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoModify" 1
    WriteRegDWORD HKCU "${UNINSTALL_KEY}" "NoRepair" 1

    ; After a quiet update, Portputer starts again.
    IfSilent 0 +2
        Exec '"$INSTDIR\${EXE}"'
SectionEnd

Section "Uninstall"
    !insertmacro WaitForPortputerToQuit
    Delete "$SMPROGRAMS\${NAME}.lnk"
    ; Start at login.
    DeleteRegValue HKCU "${RUN_KEY}" "Portputer"
    ; Only Portputer's own files, by name, in case it was installed in a
    ; folder with others. Pairings and settings stay in Portputer's data folder.
    Delete "$INSTDIR\Portputer.exe"
    Delete "$INSTDIR\portputer-agent.exe"
    Delete "$INSTDIR\nvdaControllerClient.dll"
    Delete "$INSTDIR\WinSparkle.dll"
    Delete "$INSTDIR\LICENSE.txt"
    Delete "$INSTDIR\nvdaControllerClient-LICENSE.txt"
    Delete "$INSTDIR\WinSparkle-LICENSE.txt"
    Delete "$INSTDIR\Uninstall.exe"
    RMDir "$INSTDIR"
    DeleteRegKey HKCU "${UNINSTALL_KEY}"
SectionEnd
