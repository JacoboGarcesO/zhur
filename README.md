# zhur

Terminal para Windows, al estilo de Warp: el comando se escribe en un **editor de texto real** y no en la línea del shell. Está hecha en Rust y dibuja con la GPU (DirectX 12).

## Qué hace

- **El comando se edita como texto:** clic para mover el cursor, selección con el mouse o Shift+flechas, Ctrl+C / Ctrl+V / Ctrl+Z y varias líneas con Shift+Enter.
- **Historial en bloques:** cada comando aparece como `> comando` y su salida justo debajo, sin la ruta del prompt.
- **Chips de contexto:** el shell actual, la versión de Node (en proyectos con `package.json`), la ruta, la rama de git y la cantidad de archivos cambiados.
- **Sugerencias** sacadas de tu historial, que incluye el de PowerShell. Se aceptan con → o Tab.
- **Autocompletar rutas** con Tab (si no hay ruta que completar, acepta la sugerencia). Si hay varias coincidencias, completa la parte común.
- **Copiar de la salida:** arrastra el mouse sobre el texto y presiona Ctrl+C. Si hay texto seleccionado, Ctrl+C copia en vez de interrumpir el programa.
- **Pestañas verticales:** cada una muestra un ícono con el color del shell que está corriendo (azul PowerShell, naranja WSL, verde bash, gris cmd).
- **Abre en tu carpeta de usuario** (`%USERPROFILE%`), igual que las pestañas nuevas.
- **Cualquier shell:** PowerShell, pwsh, cmd, WSL y Git Bash. Al entrar a WSL desde PowerShell, el ícono y los chips cambian solos.
- **Modo directo:** mientras corre un programa (`vim`, `python`, una contraseña), las teclas van directo a ese programa.

## Descargar

Baja `zhur-windows.zip` de [Releases](../../releases), descomprímelo y abre `zhur.exe`. No necesita instalación.

Windows mostrará "Windows protegió su PC" porque el ejecutable no está firmado. Haz clic en **Más información → Ejecutar de todas formas**.

Requisitos: Windows 10 (1809 o posterior) u 11, y una tarjeta gráfica compatible con DirectX 12.

## Uso

```
zhur.exe            # PowerShell
zhur.exe wsl
zhur.exe cmd
zhur.exe pwsh
zhur.exe "C:\Program Files\Git\bin\bash.exe"
```

| Atajo | Acción |
|---|---|
| Enter / Shift+Enter | Ejecutar / nueva línea |
| ↑ ↓ | Recorrer el historial |
| Tab | Completar ruta (si no hay, acepta la sugerencia) |
| → | Aceptar la sugerencia |
| Arrastrar + Ctrl+C | Copiar texto de la salida |
| Ctrl+Shift+T / Ctrl+Shift+W | Abrir / cerrar pestaña (también con la × que sale al pasar el mouse) |
| Ctrl+Tab | Cambiar de pestaña |

## Compilar

Necesitas [Rust](https://rustup.rs) y las Build Tools de Visual Studio con "Desarrollo de escritorio con C++".

```
cargo build --release
```

El ejecutable queda en `target\release\zhur.exe`. Incluye el runtime de C (ver `.cargo/config.toml`), así que funciona sin el Visual C++ Redistributable.

## Cómo funciona

- [`alacritty_terminal`](https://crates.io/crates/alacritty_terminal) interpreta la salida del shell, que se conecta mediante ConPTY.
- [`wgpu`](https://wgpu.rs) y [`glyphon`](https://crates.io/crates/glyphon) dibujan el texto con la GPU.
- El editor de entrada es el `Editor` de [`cosmic-text`](https://crates.io/crates/cosmic-text).
- zhur reemplaza el prompt de cada shell por uno mínimo (`> `) que además pone en el título de la ventana una marca con el shell y la carpeta actual. Así sabe cuándo el shell espera un comando y en qué carpeta está.

## Limitaciones conocidas

- Tab completa rutas, no comandos, y no muestra la lista de candidatos. En carpetas solo de WSL (`/home/...`) no completa.
- En carpetas solo de WSL (`/home/...`) no se muestra la información de git ni de Node.
- Si en WSL usas zsh en lugar de bash, zhur no detecta el prompt y funciona como una terminal clásica.
- Solo funciona en Windows.

## Licencia

[MIT](LICENSE)
