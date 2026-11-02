#!/usr/bin/env python3
"""§9.32：无头中继 + 固定端口 + 种子名单落地后，把「公网未实现」换成可证伪的真话。

4 个键 × 8 语言 = 32 处。脚本对每一条断言"原文在文件里恰好出现一次"，
少一门语言、多一个字都整体报错退出（与 §9.31 同一手法）。
"""
import io
import sys

PATH = "desktop-wallet/ui/i18n.js"

PAIRS = [
    # —— about.net（关于页组网说明；措辞闸钉住"中继 / relay"必须出现）——
    ("联邦网络：同一台 Mac 或同一 Wi-Fi 下再开一个钱包窗口，mDNS 自动发现节点、gossipsub 中继交易与区块、VRF 门票选出块人，全网账本逐块比对一致。跨公网组网需要常驻中继节点，尚未实现。",
     "联邦网络：同一台 Mac 或同一 Wi-Fi 下再开一个钱包窗口，mDNS 自动发现节点、gossipsub 中继交易与区块、VRF 门票选出块人，全网账本逐块比对一致。跨公网要靠一台常驻的中继节点（无头 --node）配固定端口与种子名单：机制已经装上，但软件不自带公网种子地址——眼下还没有一台对外常驻的机器。"),
    ("Federation: open a second wallet window on the same Mac (or another device on the same Wi-Fi). Nodes find each other via mDNS, relay transactions and blocks over gossipsub, elect the block producer with VRF tickets, and converge to one identical ledger. Crossing the public internet needs an always-on relay node — not implemented yet.",
     "Federation: open a second wallet window on the same Mac (or another device on the same Wi-Fi). Nodes find each other via mDNS, relay transactions and blocks over gossipsub, elect the block producer with VRF tickets, and converge to one identical ledger. Crossing the public internet takes an always-on relay node (headless --node) with a fixed port and a seed list; the mechanism is in place, but the app ships no public seed addresses — nothing is permanently online yet."),
    ("フェデレーション：同じ Mac（または同じ Wi-Fi 上の別デバイス）でもう一つウォレットを起動すると、mDNS で相互発見し、gossipsub で取引・ブロックを中継、VRF チケットでブロック生成者を選出し、全ノードの台帳が一致します。公網をまたぐ組網には常駐中継ノードが必要で、未実装です。",
     "フェデレーション：同じ Mac（または同じ Wi-Fi 上の別デバイス）でもう一つウォレットを起動すると、mDNS で相互発見し、gossipsub で取引・ブロックを中継、VRF チケットでブロック生成者を選出し、全ノードの台帳が一致します。公網をまたぐには固定ポートとシード名簿を持つ常駐中継ノード（ヘッドレス --node）が要ります。仕組みは実装済みですが、アプリは公網のシードアドレスを同梱しません——まだ常駐マシンが存在しないためです。"),
    ("페더레이션: 같은 Mac(또는 같은 Wi-Fi의 다른 기기)에서 지갑을 하나 더 실행하면 mDNS로 상호 발견하고, gossipsub로 거래·블록을 중계하며, VRF 티켓으로 블록 생성자를 선출해 모든 노드의 원장이 일치합니다. 공용 네트워크를 넘으려면 상주 중계 노드가 필요하며 아직 구현되지 않았습니다.",
     "페더레이션: 같은 Mac(또는 같은 Wi-Fi의 다른 기기)에서 지갑을 하나 더 실행하면 mDNS로 상호 발견하고, gossipsub로 거래·블록을 중계하며, VRF 티켓으로 블록 생성자를 선출해 모든 노드의 원장이 일치합니다. 공용 네트워크를 넘으려면 고정 포트와 시드 목록을 갖춘 상주 중계 노드(헤드리스 --node)가 필요합니다. 장치는 이미 들어가 있지만, 앱은 공용 시드 주소를 포함하지 않습니다——아직 상주 노드가 없어서입니다."),
    ("Федерация: запустите второй кошелёк на том же Mac (или другом устройстве в той же Wi-Fi-сети) — узлы находят друг друга по mDNS, передают транзакции и блоки через gossipsub, выбирают создателя блока по билетам VRF, и реестры всех узлов сходятся. Для публичной сети нужен постоянно доступный узел-ретранслятор — пока не реализовано.",
     "Федерация: запустите второй кошелёк на том же Mac (или другом устройстве в той же Wi-Fi-сети) — узлы находят друг друга по mDNS, передают транзакции и блоки через gossipsub, выбирают создателя блока по билетам VRF, и реестры всех узлов сходятся. Для публичной сети нужен постоянно доступный узел-ретранслятор (без интерфейса, --node) с фиксированным портом и списком сидов: сам механизм уже собран, но публичные сиды в поставку не входят — постоянно доступного узла пока нет."),
    ("Federación: abra una segunda ventana de la billetera en el mismo Mac (u otro equipo de la misma red Wi-Fi). Los nodos se descubren por mDNS, retransmiten transacciones y bloques por gossipsub, eligen al productor del bloque con boletos VRF y todos los libros coinciden. Cruzar a internet pública requiere un nodo retransmisor siempre activo; aún no está implementado.",
     "Federación: abra una segunda ventana de la billetera en el mismo Mac (u otro equipo de la misma red Wi-Fi). Los nodos se descubren por mDNS, retransmiten transacciones y bloques por gossipsub, eligen al productor del bloque con boletos VRF y todos los libros coinciden. Cruzar a internet pública requiere un nodo retransmisor siempre activo (--node sin interfaz) con puerto fijo y lista de semillas; el mecanismo ya está montado, pero la aplicación no incluye semillas públicas — todavía no hay ningún nodo siempre en línea."),
    ("Fédération : ouvrez une seconde fenêtre du portefeuille sur le même Mac (ou un autre appareil du même Wi-Fi). Les nœuds se découvrent par mDNS, relaient transactions et blocs via gossipsub, élisent le producteur du bloc par tickets VRF, et tous les registres concordent. Passer sur Internet public demande un nœud relais toujours actif — non implémenté.",
     "Fédération : ouvrez une seconde fenêtre du portefeuille sur le même Mac (ou un autre appareil du même Wi-Fi). Les nœuds se découvrent par mDNS, relaient transactions et blocs via gossipsub, élisent le producteur du bloc par tickets VRF, et tous les registres concordent. Traverser Internet public demande un nœud relais toujours actif (--node sans interface) avec port fixe et liste de pairs ; le mécanisme est en place, mais l’application n’embarque aucune adresse publique — aucun nœud n’est encore hébergé en permanence."),
    ("Föderation: Öffnen Sie ein zweites Wallet-Fenster auf demselben Mac (oder einem anderen Gerät im selben WLAN). Die Knoten finden sich per mDNS, leiten Transaktionen und Blöcke über gossipsub weiter, wählen den Blockersteller per VRF-Tickets — und alle Hauptbücher stimmen überein. Für das öffentliche Netz fehlt ein dauerhaft erreichbarer Relais-Knoten; noch nicht umgesetzt.",
     "Föderation: Öffnen Sie ein zweites Wallet-Fenster auf demselben Mac (oder einem anderen Gerät im selben WLAN). Die Knoten finden sich per mDNS, leiten Transaktionen und Blöcke über gossipsub weiter, wählen den Blockersteller per VRF-Tickets — und alle Hauptbücher stimmen überein. Für das öffentliche Netz braucht es einen dauerhaft laufenden Relais-Knoten (kopfloser --node-Modus) mit festem Port und Seed-Liste; die Technik ist eingebaut, aber die App liefert keine öffentlichen Seeds mit — es gibt noch keinen dauerhaft erreichbaren Knoten."),

    # —— welcome.net.note（欢迎页一句话）——
    ("联机三态见「关于」页：同机与同一 Wi-Fi 能看见彼此，公网还没实现。",
     "联机三态见「关于」页：同机与同一 Wi-Fi 能看见彼此；公网要自己接一台常驻中继节点，软件不自带种子名单。"),
    ("See the About page for the three network scopes: same machine and same Wi-Fi can see each other; the public internet is not implemented yet.",
     "See the About page for the three network scopes: same machine and same Wi-Fi can see each other; the public internet needs you to run an always-on relay node — no seed list ships with the app."),
    ("3 つの接続範囲は「概要」ページに。同一端末と同一 Wi-Fi は互いに見えますが、公衆網は未実装です。",
     "3 つの接続範囲は「概要」ページに。同一端末と同一 Wi-Fi は互いに見えますが、公衆網は自分で常駐中継ノードを立てる必要があります（シード名簿は同梱されません）。"),
    ("세 가지 연결 범위는 「정보」 페이지에 있습니다. 같은 기기와 같은 Wi-Fi는 서로 보이지만 공용 인터넷은 아직 구현되지 않았습니다.",
     "세 가지 연결 범위는 「정보」 페이지에 있습니다. 같은 기기와 같은 Wi-Fi는 서로 보이지만, 공용 인터넷은 상주 중계 노드를 직접 세워야 합니다(시드 목록은 포함되지 않습니다)."),
    ("Три режима сети описаны на странице «О программе»: один компьютер и один Wi-Fi видят друг друга, публичный интернет пока не реализован.",
     "Три режима сети описаны на странице «О программе»: один компьютер и один Wi-Fi видят друг друга; для публичной сети нужен свой постоянно доступный узел-ретранслятор — список сидов в поставку не входит."),
    ("Los tres ámbitos de red están en «Acerca de»: mismo equipo y misma Wi-Fi se ven; internet público aún no está implementado.",
     "Los tres ámbitos de red están en «Acerca de»: mismo equipo y misma Wi-Fi se ven; la internet pública exige montar tu propio nodo retransmisor siempre activo — la aplicación no incluye lista de semillas."),
    ("Les trois portées réseau sont sur la page « À propos » : même machine et même Wi-Fi se voient ; l’internet public n’est pas encore implémenté.",
     "Les trois portées réseau sont sur la page « À propos » : même machine et même Wi-Fi se voient ; Internet public exige de monter soi-même un nœud relais toujours actif — aucune liste de pairs n’est fournie."),
    ("Die drei Netzbereiche stehen auf der „Über“-Seite: gleiches Gerät und gleiches WLAN sehen sich; das öffentliche Internet ist noch nicht umgesetzt.",
     "Die drei Netzbereiche stehen auf der „Über“-Seite: gleiches Gerät und gleiches WLAN sehen sich; für das öffentliche Internet brauchen Sie einen dauerhaft laufenden Relais-Knoten — eine Seed-Liste liegt der App nicht bei."),

    # —— netstate.note（三态面板总说明）——
    ("前两档现在就能用：同机多开与同一 Wi-Fi。公网（跨网络）没有实现，界面不假装有。",
     "前两档现在就能用：同机多开与同一 Wi-Fi。公网那一档的机制已经装上（固定端口、种子名单、签名地址自报、无头中继节点），但要用起来得真有一台对外常驻的机器——软件不自带种子，界面也不假装有。"),
    ("The first two scopes work today: same machine and same Wi-Fi. The public internet is not implemented, and the UI does not pretend otherwise.",
     "The first two scopes work today: same machine and same Wi-Fi. For the public internet the machinery now exists (fixed port, seed list, signed address ads, headless relay node), but it takes a machine that is actually always online — no seeds ship with the app, and the UI does not pretend otherwise."),
    ("今使えるのは最初の 2 段階、同一端末と同一 Wi-Fi です。公衆網（ネットワーク越え）は未実装で、画面でもそう書きます。",
     "今使えるのは最初の 2 段階、同一端末と同一 Wi-Fi です。公衆網の仕組み（固定ポート・シード名簿・署名付き住所申告・ヘッドレス中継ノード）は入っていますが、本当に常時外部から見える機械が要ります。シードは同梱せず、画面は無いものを有るようには書きません。"),
    ("지금 쓸 수 있는 것은 앞의 두 단계, 같은 기기와 같은 Wi-Fi입니다. 공용 인터넷(네트워크 간)은 구현되지 않았고 화면도 그렇게 말합니다.",
     "지금 쓸 수 있는 것은 앞의 두 단계, 같은 기기와 같은 Wi-Fi입니다. 공용 인터넷을 위한 장치(고정 포트·시드 목록·서명된 주소 자기 보고·헤드리스 중계 노드)는 들어가 있지만, 실제로 항상 켜진 기계가 필요합니다. 시드는 포함되지 않고, 화면은 없는 것을 있는 척하지 않습니다."),
    ("Сейчас работают первые два режима: один компьютер и один Wi-Fi. Публичный интернет не реализован, и интерфейс это не скрывает.",
     "Сейчас работают первые два режима: один компьютер и один Wi-Fi. Механизм публичной сети (фиксированный порт, список сидов, подписанные объявления об адресах, безголовый узел-ретранслятор) уже собран, но нужен компьютер, который действительно всегда доступен извне; сидов в поставку нет, и интерфейс этого не выдумывает."),
    ("Los dos primeros ámbitos ya funcionan: mismo equipo y misma Wi-Fi. La internet pública no está implementada y la interfaz no finge lo contrario.",
     "Los dos primeros ámbitos ya funcionan: mismo equipo y misma Wi-Fi. La maquinaria de la internet pública ya está montada (puerto fijo, lista de semillas, anuncios firmados de direcciones, nodo retransmisor sin interfaz), pero hace falta una máquina siempre encendida; la aplicación no incluye semillas y la interfaz no finge lo contrario."),
    ("Les deux premières portées marchent déjà : même machine et même Wi-Fi. L’internet public n’est pas implémenté et l’interface ne fait pas semblant.",
     "Les deux premières portées marchent déjà : même machine et même Wi-Fi. Le matériel d’Internet public est en place (port fixe, liste de pairs, annonces d’adresses signées, nœud relais sans interface), mais il faut une machine réellement toujours en ligne ; l’application n’embarque aucune graine et l’interface ne fait pas semblant."),
    ("Die ersten zwei Bereiche funktionieren schon: gleiches Gerät und gleiches WLAN. Das öffentliche Internet ist nicht umgesetzt, und die Oberfläche tut nicht so.",
     "Die ersten zwei Bereiche funktionieren schon: gleiches Gerät und gleiches WLAN. Die Technik für das öffentliche Netz ist eingebaut (fester Port, Seed-Liste, signierte Adressmeldungen, kopfloser Relais-Knoten), aber es braucht einen wirklich dauerhaft laufenden Rechner; die App bringt keine Seeds mit, und die Oberfläche tut nicht so."),

    # —— netstate.public.note（公网那一行）——
    ("未实现：没有中继节点也没有公网种子，防火墙放行也连不到外网。",
     "机制已就绪：固定端口 tcp/47410、种子名单、签名地址自报、无头中继节点（--node）。缺的只是一台对外常驻的机器——软件不自带公网种子，也不会替您假装连上了。"),
    ("Not implemented: no relay nodes and no public seeds; opening the firewall still will not reach the internet.",
     "Machinery is in place: fixed port tcp/47410, seed list, signed address ads, headless relay node (--node). What is missing is a machine that stays publicly online — the app ships no public seeds and will not pretend you are connected."),
    ("未実装：中継ノードも公衆シードもありません。ファイアウォールを開けても外には出られません。",
     "仕組みは揃っています：固定ポート tcp/47410、シード名簿、署名付き住所申告、ヘッドレス中継ノード（--node）。足りないのは常時外部から見える機械だけで、アプリは公衆シードを同梱せず、接続済みようにも見せません。"),
    ("미구현: 중계 노드도 공용 시드도 없습니다. 방화벽을 열어도 외부에 닿지 않습니다.",
     "장치는 갖춰졌습니다: 고정 포트 tcp/47410, 시드 목록, 서명된 주소 보고, 헤드리스 중계 노드(--node). 부족한 것은 항상 외부에 열리는 기계뿐입니다. 앱은 공용 시드를 포함하지 않고, 연결된 척하지도 않습니다."),
    ("Не реализовано: нет ретрансляторов и публичных сидов; открытый брандмауэр всё равно не даст выхода наружу.",
     "Оборудование на месте: фиксированный порт tcp/47410, список сидов, подписанные объявления адресов, безголовый узел-ретранслятор (--node). Не хватает машины, которая всегда доступна извне; сидов в поставку нет, и интерфейс не делает вид, что вы подключены."),
    ("No implementado: sin nodos de retransmisión ni semillas públicas; abrir el cortafuegos no llega a internet.",
     "La maquinaria está lista: puerto fijo tcp/47410, lista de semillas, anuncios de direcciones firmados, nodo retransmisor sin interfaz (--node). Falta solo una máquina siempre accesible desde fuera; la aplicación no incluye semillas públicas ni finge que usted está conectado."),
    ("Non implémenté : ni nœuds relais ni graines publiques ; ouvrir le pare-feu ne mène pas sur internet.",
     "Le matériel est prêt : port fixe tcp/47410, liste de pairs, annonces d’adresses signées, nœud relais sans interface (--node). Il ne manque qu’une machine toujours accessible de l’extérieur ; l’application n’embarque aucune graine publique et ne fait pas semblant d’être connectée."),
    ("Nicht umgesetzt: keine Relay-Knoten und keine öffentlichen Seeds; offene Firewall erreicht trotzdem kein Internet.",
     "Die Technik ist da: fester Port tcp/47410, Seed-Liste, signierte Adressmeldungen, kopfloser Relais-Knoten (--node). Es fehlt nur ein Rechner, der dauerhaft von außen erreichbar ist; die App liefert keine öffentlichen Seeds mit und tut nicht so, als wären Sie verbunden."),
]


def main():
    src = io.open(PATH, encoding="utf-8").read()
    fails = []
    for old, new in PAIRS:
        n = src.count(old)
        if n != 1:
            fails.append("命中 %d 次（应为 1 次）：%s…" % (n, old[:40]))
            continue
        src = src.replace(old, new, 1)
    if fails:
        print("翻红 %d 项：" % len(fails))
        for f in fails:
            print(" ·", f)
        sys.exit(1)
    io.open(PATH, "w", encoding="utf-8").write(src)
    print("✓ 已替换 %d 处（4 键 × 8 语言），逐条断言原文唯一" % len(PAIRS))


if __name__ == "__main__":
    main()
