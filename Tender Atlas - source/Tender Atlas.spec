# -*- mode: python ; coding: utf-8 -*-


a = Analysis(
    ['atlas.py'],
    pathex=[],
    binaries=[],
    datas=[('static', 'static'), ('countries.json', '.'), ('roots.pem', '.'), ('CA-LICENSE.txt', '.'), ('discovery/examples.json','discovery')],
    hiddenimports=[],
    hookspath=[],
    hooksconfig={},
    runtime_hooks=[],
    excludes=[],
    noarchive=False,
    optimize=0,
)
pyz = PYZ(a.pure)

exe = EXE(
    pyz,
    a.scripts,
    a.binaries,
    a.datas,
    [],
    name='Tender Atlas',
    debug=False,
    bootloader_ignore_signals=False,
    strip=False,
    upx=False,
    upx_exclude=[],
    runtime_tmpdir=None,
    console=False,
    disable_windowed_traceback=False,
    argv_emulation=False,
    target_arch=None,
    codesign_identity=None,
    entitlements_file=None,
    icon=['atlas.ico'],
)
