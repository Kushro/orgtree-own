; Recursos del motor (#24), incluidos dentro de la seccion "Install" de
; installer/template.nsi. Los arma installer/stage_resources.py en
; target/bundle-resources con la misma disposicion que `extraResources` de
; Electron: engine/ (con runtime/, postgresql/ y pg-custodian.exe),
; tools/pypg/ y build-info.json. La app los busca en <instalacion>\resources.
;
; /nonfatal: un `dx bundle` local sin los recursos armados igual genera el
; instalador (sin motor); el CI verifica la instalacion completa.
SetOutPath "$INSTDIR\resources"
File /nonfatal /r "${__FILEDIR__}\..\target\bundle-resources\*.*"
