# Plano: um navegador para o E72

## Resumo

Um navegador **fim-a-fim** — o aparelho fala direto com o site, sem proxy nosso no meio.
Nada de motor escrito do zero: o TLS vem de uma biblioteca já portada para Symbian, o
parsing de HTML e CSS vem das bibliotecas MIT do NetSurf, os codecs de imagem vêm do
próprio aparelho. O que escrevemos é a cola, o layout e a casca — que é justamente a parte
onde uma tela de 320×240 exige um comportamento que nenhum motor pronto tem.

TLS entra em dois degraus: **1.2 primeiro**, porque já está pronto e destrava o produto
inteiro, e 1.3 depois, atrás da mesma interface.

## Decisões

| Decisão | Escolha | Por quê |
|---|---|---|
| Arquitetura de rede | **Fim-a-fim.** Sem proxy/transcoder | Decisão do tutor: ninguém no meio vê o texto claro |
| TLS | `mbedtls.dso` (mbedTLS 3.4.1 portado para Symbian) | É `.dso`, linkamos como qualquer DLL do SDK |
| Degrau 1 | **TLS 1.2**, alvo padrão do port | Pronto. Destrava HTTP, layout, UI — 90% do projeto |
| Degrau 2 | TLS 1.3, alvo `mbedtls_tls13.mmp` | Troca de build, não de código: fica atrás do trait `Tls` |
| Camada de rede do mbedTLS | **Não usar.** `mbedtls_ssl_set_bio` com callbacks nossos | Evita PIPS e mantém tudo orientado a evento sobre `net.rs` |
| Store de certificados | `cacert.pem` do bundle da curl.se, versionado aqui | Arquivo que a gente troca, não dívida perpétua |
| HTML/CSS | **libhubbub + libdom + libcss** (NetSurf, MIT) | Estado da arte para hardware fraco; a parte mais subestimada (cascade) vem pronta |
| Core do NetSurf (`render/`, `content/`) | **Não.** É GPL-2.0 | Este repo é MIT. E o layout dele não é o que queremos (ver abaixo) |
| Layout | **Nosso**, em Rust, sobre os estilos computados do libcss | Numa tela de 320px, layout fiel é o comportamento *errado* |
| Imagens | `symbian::image` — ICL do aparelho | JPEG/PNG/GIF/BMP de graça. Sem WebP, e o chamador tem que saber disso |
| gzip/deflate | `symbian_crypto::inflate` | Já existe e já tem vetores de teste |
| JavaScript | **Fora de escopo.** Reavaliar com duktape depois | É outro projeto inteiro |
| HTTP | 1.1 apenas, nosso, em Rust | HTTP/2 exige ALPN + HPACK + multiplexação; não paga aqui |
| Fronteira do render | Uma **Page IR** (display list) entre layout e pintura | Testável no `symbian-preview`, sem aparelho |

## O que já existe e não precisa ser escrito

Inventário medido neste repo, não lembrado:

| Peça | Onde |
|---|---|
| TCP/DNS/bearer orientado a evento, não-bloqueante | `crates/symbian/src/net.rs` |
| inflate / zlib / gzip | `crates/symbian-crypto/src/inflate.rs` |
| SHA-256/512, HMAC, AES, bignum, DRBG, `ct_eq` | `crates/symbian-crypto` |
| Canvas 16bpp, clip, damage, blit/mask, fontes bitmap com fallback | `crates/symbian-gfx` |
| Chrome, list, edit, viewer, tabs, input, theme | `crates/symbian-ui` |
| Decode de imagem assíncrono pelos codecs do aparelho | `crates/symbian/src/image.rs` |
| Cache em disco no data cage | `crates/symbian/src/cache.rs` |
| Trabalho em thread de fundo (`SHIM_EV_WORK_DONE`) | `shim_work.cpp` |
| Log/trace ao vivo | `symbian::log!` -> `C:\Data\_logs\<app>.txt`, lido com `epoc logs -f` |
| Toolchain que cruza C e C++ para `armv5te-symbian-eabi` | `docs/build-flow.md` |

Esse último item é o que torna o plano possível: **já compilamos C para o aparelho**. Nem
mbedTLS nem as libs do NetSurf são um toolchain novo.

## Por que fim-a-fim custa, e o que se perde

Foi decidido, mas fica registrado o preço para ninguém se surpreender depois:

- **Sem JavaScript**, boa parte da web atual é tela branca. Não é limitação de TLS, é de motor.
- **Sem transcoder**, uma página moderna são 2–3 MB e 70+ requisições que chegam inteiras
  no aparelho. O E72 tem 3G/HSDPA e WiFi, então banda dói menos que em 2005 — o que aperta
  é RAM (~50 MB livres) e latência de 70 conexões.
- **Cada site quebrado é bug nosso para sempre**, sem a válvula de escape de consertar no
  servidor.

O que compra: privacidade real, zero infra, zero custo por usuário, funciona em intranet e
captive portal, e não vira tijolo no dia em que um servidor nosso morrer — que é
literalmente o que aconteceu com o Opera Mini.

## Arquitetura

```
┌────────────────────────────────────────────────────────────────────┐
│ apps/browser        chrome, barra de URL, scroll, links, histórico │
└───────────────┬────────────────────────────────────────────────────┘
                │ consome Page IR
┌───────────────▼──────────────┐   ┌────────────────────────────────┐
│ symbian-layout    (NOVO,rs)  │   │ symbian-gfx / symbian-ui       │
│ caixas, fluxo, fit-to-width  │──►│ (existe) pinta a IR            │
└───────────────▲──────────────┘   └────────────────────────────────┘
                │ estilos computados + DOM
┌───────────────┴──────────────────────────────────────┐
│ symbian-dom      (NOVO, binding rs → C)              │
│   libhubbub  HTML5 → libdom  DOM → libcss  cascade   │  MIT
│   + libparserutils, libwapcaplet                     │
└───────────────▲──────────────────────────────────────┘
                │ bytes + charset
┌───────────────┴──────────────┐
│ symbian-http     (NOVO, rs)  │  1.1, chunked, gzip✓, redirect, cache✓
└───────────────▲──────────────┘
                │ stream de bytes
┌───────────────┴──────────────────────────────────────┐
│ symbian-tls      (NOVO)   trait Tls                  │
│   backend: shim_tls.cpp → mbedtls_ssl_set_bio        │
│            └─ send/recv = TcpStream do net.rs        │
│   degrau 1: TLS 1.2      degrau 2: TLS 1.3           │
└───────────────▲──────────────────────────────────────┘
                │
┌───────────────┴──────────────┐
│ symbian::net  (existe)       │  RSocket, event-driven, não-bloqueante
└──────────────────────────────┘
```

### Por que o layout é nosso

O layout do NetSurf é CSS 2.1 conforme: floats, tabelas, inline-block. Duas coisas o
descartam. É GPL-2.0, e — mais importante — **numa tela de 320 px, renderizar fielmente é
o comportamento errado**. O que faz um navegador ser usável nesse aparelho é o que o Opera
Mini fazia: ignorar a largura declarada, refluir tudo para a largura da tela, colapsar
colunas em uma. Isso não é um motor conforme com um ajuste; é uma política de layout
diferente. Escrevê-la é o produto, não o custo.

O que **não** escrevemos é o que as pessoas subestimam: tokenizer HTML5 tolerante a erro
(libhubbub), construção de DOM (libdom), parsing de CSS + matching de seletor + cascata
com especificidade e herança (libcss). Esse é o trabalho de anos que estamos pegando pronto.

### Por que não a camada de rede do mbedTLS

O port avisa que o alvo ESTLIB exclui o módulo de networking, e o alvo PIPS arrasta o
Open C. Nada disso é preciso: `mbedtls_ssl_set_bio` recebe um par de callbacks nossos, e a
gente pluga o `TcpStream` do `net.rs` direto. `mbedtls_ssl_handshake` devolve
`MBEDTLS_ERR_SSL_WANT_READ`/`WANT_WRITE`, que encaixa exatamente no pump de eventos — e
respeita a lição que já custou caro aqui: **nada pode bloquear a thread da UI**
(ver `shim-pump-event-driven`). O RNG (`f_rng`) sai de `symbian::random` + `Drbg`.

### A Page IR

A fronteira entre layout e pintura é um display list resolvido: retângulos preenchidos,
runs de texto com fonte/cor/posição já decididas, imagens por handle, retângulos de link
com destino. Duas razões:

1. O layout roda em thread de fundo (`shim_work`) e entrega uma IR imutável; a thread da
   UI só pinta. Scroll fica fluido porque scroll não reflui.
2. A IR é serializável, então o `symbian-preview` renderiza páginas no desktop e o
   layout ganha testes de verdade — sem aparelho, sem rede.

## Fases

| # | Entrega | Risco que mata | Sai com |
|---|---|---|---|
| **F1** | Probes isolados: `mbedtls.dll` carrega? Open C `libc` carrega? Handshake TLS 1.2 real, cronometrado no ARM11 | R1, R2 | um relatório do aparelho |
| **F2** | `symbian-tls` + `shim_tls.cpp`. BIO sobre `net.rs`, orientado a evento | | `GET https://…` imprime bytes no log |
| **F3** | `symbian-http`: 1.1, chunked, gzip, redirect, cookies mínimos, sobre `cache.rs` | | baixa e cacheia uma página |
| **F4** | Cross-compile de libwapcaplet/libparserutils/libhubbub/libdom/libcss para armv5 | R1 | dump da árvore DOM+estilos no log |
| **F5** | `symbian-layout`: cascade → caixas → Page IR, fit-to-width | R3 | páginas renderizadas **no preview**, sem aparelho |
| **F6** | `apps/browser`: chrome, URL, scroll, links, histórico, imagens via ICL | R3, R5 | o navegador |
| **F7** | TLS 1.3: troca do alvo de build + `cacert.pem` | | nada acima do trait `Tls` muda |
| **F8** | Modo leitura (extração tipo readability) | R4 | a única forma decente de ler notícia em 320px |

F1 é meia hora e decide se o resto existe. F5 é a maior fatia de código nosso e a única
que roda inteira no desktop.

## Riscos, e o que mede cada um

- **R1 — runtime C.** As libs do NetSurf e o mbedTLS precisam de `malloc`, `str*`,
  `snprintf`. No E72 o `libc` do Open C **carrega** (medido em `docs/device-notes.md`),
  mas isso é propriedade do aparelho, não do SDK. Plano B é um libc mínimo sobre
  `User::Alloc` e o alocador do Rust. *Mede: probe isolado em F1.*
- **R2 — `mbedtls.dso` é um import.** Import que não resolve faz o app **sumir sem panic,
  sem log e sem report** — a regra já documentada aqui. Um import arriscado por binário,
  sempre (`device-probes-must-be-isolated`). Se não estiver no aparelho, instalamos o SIS
  do nnproject ou embutimos a lib (Apache-2.0 permite). *Mede: F1.*
- **R3 — RAM.** ~50 MB livres. DOM + CSSOM + IR de página real pode estourar. Mitigação:
  IR em arena, descarte de subárvores fora de vista, limite duro de bytes por documento
  com erro honesto em vez de OOM. *Mede: F5 no preview, F6 no aparelho.*
- **R4 — CSS moderno.** libcss cobre CSS 2.1 e parte de 3; flexbox e grid, que a web atual
  usa em tudo, não têm cascade nem layout aqui. Muitas páginas vão sair torta. É o motivo
  de F8 existir. *Mede: uma lista fixa de 20 sites, reavaliada a cada fase.*
- **R5 — reflow travando a UI.** Layout tem que ir para `shim_work`. Se voltar para a
  thread da UI "só por enquanto", vira o bug do `CIdle` de novo.
- **R6 — licença.** Só as libs MIT. Tocar em `render/` ou `content/` do NetSurf muda a
  licença deste repo inteiro para GPL-2.0.

## Explicitamente fora de escopo

JavaScript, proxy/transcoder, WebP, HTTP/2 e /3, vídeo, downloads, múltiplas abas,
formulários além de GET/POST simples, e conformidade com qualquer spec.

## Fontes externas

- [nnproject — Symbian TLS](https://nnproject.cc/tls/) — patch `ssl.dll` v19 e a lib
  mbedTLS v1.4.2; CA roots atualizados em 2026-02-11
- [shinovon/mbedtls-symbian](https://github.com/shinovon/mbedtls-symbian) — mbedTLS 3.4.1
  para Symbian 9.1+, alvo `mbedtls_tls13.mmp`, Apache-2.0
- [NetSurf](https://www.netsurf-browser.org/) — navegador GPL-2.0; libhubbub, libcss,
  libdom, libparserutils, libwapcaplet sob MIT
