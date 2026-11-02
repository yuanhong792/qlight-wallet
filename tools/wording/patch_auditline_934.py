#!/usr/bin/env python3
"""把 about.auditline 那句"账本被改动会拒绝出块并停止收单"换成代码真做的事：
哈希链校验不过 → 那本旧文件归档（不删）→ 从创世重来。§9.30 实测的就是这个行为，
而 UI 里 8 种语言都在念另一句。"""
import io, re

P = "/Volumes/yuanhong/量子货币/desktop-wallet/ui/i18n.js"
NEW = [
    ("每 2 秒", "每 2 秒出块、块哈希绑定全部交易；启动时逐块重放校验哈希链，账本被改动就把那本旧文件归档（不删）并从创世重来——多拿不到一个币。"),
    ("Blocks every", "Blocks every 2s; the block hash binds every included transaction. On start the hash chain is replayed and verified — a tampered ledger is archived (never deleted) and this node restarts from genesis, so editing the file mints nothing."),
    ("2秒ごとに", "2 秒ごとにブロック生成、ブロックハッシュは全取引と連結。起動時にハッシュチェーンを再検証し、改ざんされた台帳は（削除せず）アーカイブして創世からやり直します——ファイルを書き換えても 1 コインも増えません。"),
    ("2초마다", "2초마다 블록 생성, 블록 해시는 모든 거래와 연결. 시작 시 해시 체인을 다시 검증하며, 조작된 원장은 삭제하지 않고 보관한 뒤 제네시스부터 다시 시작합니다 — 파일을 고쳐도 코인은 늘지 않습니다."),
    ("Блок каждые", "Блок каждые 2 с; хэш блока связывает все его транзакции. При запуске хэш-цепь проигрывается и проверяется — изменённый реестр архивируется (не удаляется) и узел начинает с генезиса: правка файла не добавляет ни монеты."),
    ("Bloques cada", "Un bloque cada 2 s; el hash del bloque vincula todas sus transacciones. Al iniciar se verifica la cadena de hash: un libro alterado se archiva (nunca se borra) y el nodo reinicia desde el génesis — editar el archivo no crea ninguna moneda."),
    ("bloc toutes les", "Un bloc toutes les 2 s ; le hachage du bloc lie toutes ses transactions. Au démarrage la chaîne est rejouée et vérifiée — un registre modifié est archivé (jamais supprimé) et le nœud repart du genèse : éditer le fichier ne crée aucune pièce."),
    ("Blöcke alle", "Blöcke alle 2 s; der Blockhash bindet jede enthaltene Transaktion. Beim Start wird die Hash-Kette neu gespielt und geprüft — ein verändertes Hauptbuch wird archiviert (nie gelöscht) und der Knoten startet neu bei der Genesis: die Datei zu ändern prägt keine Münze."),
]

lines = io.open(P, encoding="utf-8").read().split("\n")
out, hits = [], 0
for line in lines:
    m = re.match(r'^(\s*)"about\.auditline":\s*"(?:[^"\\]|\\.)*"(,?)\s*$', line)
    if m:
        indent, comma = m.group(1), m.group(2)
        for marker, text in NEW:
            if marker in line:
                esc = text.replace("\\", "\\\\").replace('"', '\\"')
                line = f'{indent}"about.auditline": "{esc}"{comma}'
                hits += 1
                break
        else:
            print("认不出语言，原样保留:", line[:80])
    out.append(line)
io.open(P, "w", encoding="utf-8").write("\n".join(out))
print("改写条数 =", hits, "（应为 8）")
