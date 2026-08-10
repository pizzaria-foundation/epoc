# Mensagem de voz: o que está pronto e o que falta

Estado em 9 de agosto de 2026, ao pausar. Escrito para que retomar não custe reconstruir
contexto — e para separar com honestidade **o que está medido** do que ainda é suposição.

---

## O que está pronto e verificado

### No host, com teste (683 testes verdes no workspace)

| Peça | Onde | O que garante |
|---|---|---|
| Demux Ogg | `apps/telegram/src/ogg.rs` | 14 testes. Pacote atravessando páginas, pacote de exatamente 255 bytes, página perdida, arquivo truncado, `OpusHead` |
| Duração | `ogg::granule_end`, `ogg::duration_ms` | Lê o granule da última página, sem decodificar |
| Decode Opus | `crates/opus/` + `apps/telegram/src/opus.rs` | 6 testes. Decodifica um arquivo real e **recupera o tom de 440 Hz** |
| Writer WAV | `apps/telegram/src/wav.rs` | 8 testes, mais validação externa por `ffprobe` e `ffmpeg` |
| libopus vendorizado | `vendor/libopus/` | Compila para ARM **e** para host, decode-only, fixed-point |

O teste que mais vale é `the_decoded_audio_is_the_tone_that_was_encoded`: comprimento
sozinho passaria para um buffer de silêncio, então ele conta cruzamentos de zero para
recuperar o pitch e confere o pico de amplitude, porque ruído em torno de zero passaria no
teste de pitch. A duração decodificada também bate com o granule — duas partes
independentes do formato concordando.

### No aparelho, medido

- **`shim_audio.cpp` funciona.** A linha A do `examples/audioprobe` abriu um WAV PCM16 de
  8 kHz mono escrito pela própria aplicação, reportou 900 ms para um clipe de 900 ms
  (então a plataforma **não** substituiu a taxa), tocou, e **foi ouvido**.
- O `audioprobe` exercita o `shim_audio.cpp` de verdade, não uma cópia — então essa linha
  valida o código que vai para produção.

### Registrado em `docs/device-notes.md`

Os achados do libopus (sem libm, 107 KB, `malloc`/`free` necessários), a armadilha do
`-DOPUS_ARM_ASM=0`, o caso do `_reg.rss` ausente, e as duas pegadinhas do
`CMdaAudioPlayerUtility` (`Stop()` não chama de volta; a preferência padrão falha em vez de
degradar).

---

## O bloqueio de desenho que ainda não tem solução escrita

**A decodificação não cabe na thread worker como está.** Isto não é um detalhe de
implementação, é a decisão que falta tomar, e está documentada em `shim/src/shim_work.cpp`:

> Com heap próprio, o alocador global do Rust no worker resolve para o RHeap do worker,
> porque `shim_alloc` chama `User::Alloc`, que é **por thread**. O que não é aceitável é
> uma alocação *escapar* — um `Vec` construído no worker e destruído na thread de GUI é um
> free entre heaps, que é corrupção silenciosa e não uma falha limpa.
>
> O contrato é: nada que o job aloca pode sobreviver a ele.

E `opus::decode_stream` faz exatamente o proibido: **devolve um `Vec<i16>`**. Uma mensagem
de voz de 30 segundos são 1,4 milhão de amostras, ~2,8 MB — grande demais para o buffer do
chamador ser pré-alocado com folga, e é justamente o tipo de alocação que não pode cruzar.

Três saídas possíveis, sem uma escolhida ainda:

1. **O job escreve o WAV direto no disco**, e nada volta em memória. O `wav::header()` já
   existe separado de `wav::file()` exatamente para isso — escrever o cabeçalho, depois
   fazer streaming das amostras. Custa saber a contagem de amostras antes, que o granule
   dá. Parece a melhor, e é a que o plano original previa.
2. **Buffer de saída do chamador**, alocado na GUI antes de submeter. Consistente com o
   resto do `shim_work`, mas exige dimensionar pelo pior caso.
3. **Decodificar na thread de GUI em fatias**, um punhado de pacotes por passo do pump.
   Elimina o problema de heap inteiro, mas precisa que o decode seja rápido — e **isso
   ainda não foi medido no aparelho**.

Nada disso é decidível sem o item seguinte.

---

## O que falta, em ordem

### 1. Rodar as linhas B a E do `audioprobe` (precisa do aparelho)

Já está instalado. Reabrir o app roda a próxima linha; relatório em `C:\Data\audioprobe.txt`.

| Linha | O que pergunta | O que muda conforme a resposta |
|---|---|---|
| **B** | 48 kHz mono toca? | **Decide se preciso escrever um resampler.** Opus sempre decodifica a 48 kHz. Se tocar, as amostras vão direto para o disco |
| C | 16 kHz mono | A taxa de fallback, se B falhar |
| **D** | mesma coisa que B, mas do diretório privado | **Decide onde escrever o WAV.** Se o MMF não ler do data cage, o arquivo tem de ir para `C:\Data\` e fica visível ao usuário |
| E | 44,1 kHz estéreo | Só cobertura; nenhuma decisão depende |

Lembrar: **escutar**. O relatório não ouve, e uma linha pode reportar sucesso e sair muda —
por isso cada linha toca um tom de pitch próprio, e um tom no pitch errado denuncia uma
taxa substituída que a plataforma reportaria como sucesso.

### 2. Medir o decode no aparelho (precisa do aparelho)

Não existe número nenhum. Um ARM1136 de 600 MHz decodificando Opus fixed-point pode estar
acima ou abaixo do tempo real, e isso decide entre a saída 1 e a saída 3 acima, além de
decidir se a interface precisa de indicador de progresso ou só de uma espera curta.

Falta um `examples/opusprobe`, ou uma linha extra no `audioprobe`, que decodifique um
`.opus` embutido e reporte **ms por segundo de áudio**. É pequeno: o decoder já compila
para ARM, e o padrão de uma linha por abertura já está escrito duas vezes.

### 3. Ligar o fluxo no cliente

Hoje **nada chama `decode_stream`**. As peças existem e não se falam. Falta:

- Ramificar por tipo de mídia ao abrir uma mensagem de voz (baixar o documento em pedaços
  já funciona — é o mesmo caminho da foto).
- Submeter o decode ao worker, na forma que o item 1 do bloqueio resolver.
- Escrever o WAV no cache por id, ao lado do que `media_cache.rs` já faz para imagem.
- `shim_audio_open_file` → esperar `SHIM_EV_AUDIO_OPENED` → `shim_audio_play`.
- Play/pause no D-pad, progresso na bolha via `shim_audio_position_ms`.
- Desenhar a onda de verdade: `documentAttributeAudio.waveform` já é lido e guardado
  (`Media::Document { waveform }`), 5 bits empacotados por amostra, e ninguém desenha.

### 4. Dívidas carregadas das fases anteriores

- **O Telegram ainda decodifica imagem do caminho de diagnóstico.**
  `apps/telegram/src/lib.rs:1053` lê `C:\Data\imgprobe-input.jpg` em vez do cache por id.
  Isso ficou de quando o decode não completava e era preciso um arquivo conhecido. Trocar
  — e **verificar no aparelho**, não assumir.
- **Linhas C, D e F do `imgprobe` nunca rodaram.** São `EColor64K`, tamanho reduzido e
  `DataNewL`. As três foram culpadas enquanto a causa real era o pump, e estão marcadas
  como **desconhecidas** no `device-notes.md`. Se `EColor64K` funcionar, some a conversão
  de 24 bpp para RGB565 em todo decode de imagem.
- **Fase 5, testabilidade.** Injetar `Images` no `App` para que `Screen::Viewer` entre em
  `drawing_every_screen_stays_inside_the_framebuffer`. Hoje o viewer é a única tela que
  nenhum teste desenha. `MemImages` já existe; falta a injeção.

---

## Fora de escopo, de propósito

Decidido com você e não mudou:

- **Enviar mídia** — foto e voz. Precisa de `upload.saveFilePart`, `messages.sendMedia`,
  seletor de arquivo, e para voz microfone com capability `UserEnvironment` mais o
  **encoder** Opus além do decoder. Plano seguinte.
- **Sticker com pixels** — sem decoder WebP; a bolha com emoji grande é a solução, e
  funciona.
- Sticker animado (`.tgs`, `.webm`), CDN, vídeo, GIF animado, orientação EXIF.

---

## Onde as coisas estão

```
crates/opus/                     wrapper seguro; segura o unsafe porque tg é forbid(unsafe_code)
vendor/libopus/upstream/         fontes decode-only, libopus 1.4
vendor/libopus/compat/           os headers de libc que não existem neste toolchain
vendor/libopus/build.sh          device | host — as flags e o porquê de cada uma
apps/telegram/src/ogg.rs         demux
apps/telegram/src/opus.rs        decode_stream (o que devolve Vec — ver o bloqueio)
apps/telegram/src/wav.rs         header() separado de file(), para streaming
apps/telegram/src/testdata/      voice.opus, gerado por ffmpeg/libopus
shim/src/shim_audio.cpp          CMdaAudioPlayerUtility
examples/audioprobe/             a matriz, uma linha por abertura
```

Reconstruir o libopus depois de mexer: `bash vendor/libopus/build.sh device` e
`bash vendor/libopus/build.sh host`. O `build.rs` da crate avisa se faltar, em vez de
deixar o linker listar quarenta símbolos indefinidos.

---

## A regra que este ciclo confirmou

De `docs/device-notes.md`, e vale reler antes de retomar:

> Numa plataforma sem debugger, sem console e sem log, **construa o instrumento em vez de
> adivinhar.**

Duas vezes nesta rodada eu quase relatei um sucesso que não existia. O link do libopus
"passou" sem um símbolo indefinido — porque o `--gc-sections` tinha descartado o decoder
inteiro, já que ninguém o chamava; `nm | grep -c opus` deu 0. E o `audioprobe` instalou,
reportou sucesso em cada etapa, e não estava no menu.

Nos dois casos a diferença entre acreditar e saber foi uma medição de trinta segundos.
