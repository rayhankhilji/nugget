#!/usr/bin/env python3
"""Synthetic mailbox for JJ1's ingest-triage scenarios (HH2).

Writes N .eml files (default 2000) under OUT/mail/<folder>/, plus OUT/truth.jsonl with one line per
file: {"path", "lang", "class": "sensitive"|"junk"|"normal", "kind": "health"|"financial"|
"credentials"|"legal"|None, "probe": optional exact phrase to search for}.

Every message is written for this package (fictional people, `.example` domains). The planted
sensitive and junk items are phrased the way real mail is, not with keyword lists: a rule set that
only knows "therapy" should miss "Ihr Termin in der Praxis Dr. Kaya", and that is what we measure.
Languages: en, de, es, fr (sensitive and junk), plus ordinary mail in all four.
Deterministic: --seed fixes the corpus; dates are spread over 2026-01..2026-10 (absolute).
"""
import argparse
import json
import os
import random
from email.utils import format_datetime
from datetime import datetime, timedelta, timezone

# (lang, kind, subject, body, probe) — probe is a distinctive phrase only this item contains.
SENSITIVE = [
    ("en", "health", "Your lab results are ready", "Hi Robin,\n\nYour blood test from Tuesday came back. Your HbA1c is 6.9, a little higher than last time. Dr. Okafor would like to see you before adjusting the metformin dose. Please book a slot this week.\n\nWillow Lane Practice", "Willow Lane Practice"),
    ("en", "health", "Follow-up after Thursday's session", "Robin, thanks for being so open on Thursday. As we discussed, try the breathing exercise when the panic starts at work, and keep the mood journal. Same time next week?\n\nMarta", "mood journal"),
    ("en", "financial", "Statement for account ending 4471", "Your monthly statement is attached. Closing balance: GBP 2,318.40. Overdraft limit: GBP 500. A direct debit of GBP 912.00 to Harbour Lettings was paid on 1 March.", "Harbour Lettings"),
    ("en", "financial", "Your payslip for September", "Gross pay 4,150.00, tax 812.40, pension 207.50, net pay 3,130.10. Paid to sort code 20-44-91.", "net pay 3,130.10"),
    ("en", "credentials", "Your new login for the tenant portal", "Username: robin.hale\nTemporary password: Kestrel!Moon42\nYou will be asked to change it on first sign-in.", "Kestrel!Moon42"),
    ("en", "credentials", "Wi-Fi for the cabin", "Network: Pinecrest-Guest, passphrase: amber-otter-1182. The router is behind the bookshelf if it needs a restart.", "amber-otter-1182"),
    ("en", "legal", "Re: tenancy dispute — next steps", "Robin, I have reviewed the landlord's letter. We have grounds to contest the deposit deduction. I would suggest we file at the small claims court before 14 November. My fee for drafting the claim is fixed.\n\nJ. Whitlock, Solicitor", "small claims court"),
    ("en", "legal", "Custody arrangement draft", "Attached is the draft parenting plan for Ella: alternate weekends, half of school holidays. Please read it before mediation on the 22nd.", "parenting plan for Ella"),
    ("de", "health", "Ihr Termin in der Praxis Dr. Kaya", "Guten Tag Frau Hale,\n\nIhr Befund ist da: die Schilddrüsenwerte sind leicht erhöht. Frau Dr. Kaya möchte die Dosis des L-Thyroxin anpassen. Bitte vereinbaren Sie einen Termin zur Besprechung.\n\nPraxis am Hafen", "Schilddrüsenwerte"),
    ("de", "health", "Krankmeldung bis Freitag", "Hallo Jonas, ich bin bis Freitag krankgeschrieben, die AU-Bescheinigung schicke ich der Personalabteilung. Die Physiotherapie für den Rücken beginnt nächste Woche.", "AU-Bescheinigung"),
    ("de", "financial", "Kontoauszug Oktober", "Ihr Kontostand am 31.10.: 1.204,55 EUR. Dispositionskredit: 1.000 EUR. Die Lastschrift der Hausverwaltung Brenner über 980,00 EUR wurde ausgeführt.", "Hausverwaltung Brenner"),
    ("de", "financial", "Gehaltsabrechnung September", "Brutto 4.800,00 EUR, Lohnsteuer 802,16 EUR, Netto 2.951,30 EUR. Überweisung auf Ihr Girokonto.", "Netto 2.951,30"),
    ("de", "credentials", "Zugangsdaten für das Mieterportal", "Benutzername: r.hale\nKennwort: Lindenblatt#77\nBitte ändern Sie das Kennwort nach der ersten Anmeldung.", "Lindenblatt#77"),
    ("de", "legal", "Ihre Klage gegen die Hausverwaltung", "Sehr geehrte Frau Hale, die Klageschrift wurde beim Amtsgericht eingereicht. Der Gütetermin ist am 3. Dezember. Bitte bringen Sie den Mietvertrag mit.\n\nRechtsanwältin Petra Vogel", "Gütetermin"),
    ("es", "health", "Resultados de su análisis", "Estimada Sra. Hale: los resultados de su analítica muestran el colesterol en 245. La doctora Ruiz le recomienda una consulta para valorar el tratamiento.\n\nCentro de Salud Las Arenas", "Centro de Salud Las Arenas"),
    ("es", "financial", "Extracto de su cuenta", "Saldo a 30 de septiembre: 3.112,08 EUR. Recibo de la hipoteca cargado: 845,00 EUR. Tarjeta terminada en 9921.", "Recibo de la hipoteca"),
    ("es", "credentials", "Tu contraseña del portal del colegio", "Usuario: rhale_familia\nContraseña: Gaviota*2026\nNo la compartas con nadie.", "Gaviota*2026"),
    ("es", "legal", "Demanda laboral — citación", "Le informamos de que el juicio por despido está señalado para el 18 de noviembre en el Juzgado de lo Social n.º 4. Traiga el contrato y las nóminas.\n\nAbogada Lucía Ferrer", "Juzgado de lo Social"),
    ("fr", "health", "Compte rendu de consultation", "Madame Hale, suite à votre consultation, le Dr Benali confirme une hernie discale L4-L5. Une IRM de contrôle est prescrite dans trois mois. Arrêt de travail de deux semaines.", "hernie discale"),
    ("fr", "financial", "Votre relevé de compte", "Solde au 30/09 : 2 487,12 EUR. Prélèvement du loyer : 1 150,00 EUR. Découvert autorisé : 800 EUR.", "Découvert autorisé"),
    ("fr", "credentials", "Vos identifiants pour l'espace client", "Identifiant : r.hale\nMot de passe provisoire : Mouette+3391\nMerci de le modifier lors de votre première connexion.", "Mouette+3391"),
    ("fr", "legal", "Convocation au tribunal", "Madame, vous êtes convoquée devant le tribunal judiciaire le 9 décembre pour l'affaire du dépôt de garantie. Maître Girard vous représentera.", "dépôt de garantie"),
]

JUNK = [
    ("en", "Flash sale: 70% off everything this weekend only", "Don't miss out! Use code SAVE70 at checkout. Unsubscribe | View in browser", "SAVE70"),
    ("en", "You've been selected for an exclusive reward", "Congratulations! Claim your gift card now before it expires. Click here. Unsubscribe.", "exclusive reward"),
    ("en", "Weekly digest: 12 new posts you might like", "Top stories from communities you follow. Manage notification settings.", "communities you follow"),
    ("de", "Nur heute: 50 % auf alle Sneaker", "Jetzt zuschlagen! Gutscheincode SNEAK50. Newsletter abbestellen.", "SNEAK50"),
    ("de", "Ihr persönlicher Gewinn wartet", "Herzlichen Glückwunsch, Sie haben gewonnen! Jetzt klicken und Preis sichern. Abmelden.", "Preis sichern"),
    ("es", "Ofertas de otoño: hasta -60 %", "Solo esta semana. Código OTONO60. Darse de baja.", "OTONO60"),
    ("fr", "Soldes privées : -40 % sur la maison", "Offre réservée aux membres. Code MAISON40. Se désabonner.", "MAISON40"),
]

NORMAL = {
    "en": [("Lunch on Thursday?", "Are you free for lunch on Thursday? The new noodle place on Garnet Street opened."),
           ("Slides for the review", "Here are the slides for Monday's design review. Comments welcome before Friday."),
           ("Bike repair", "The shop says the wheel will be ready Wednesday afternoon."),
           ("Book club pick", "Next month we're reading a novel about lighthouse keepers. Hosting at mine.")],
    "de": [("Grillen am Samstag", "Wir grillen am Samstag ab 17 Uhr im Garten. Bring gern einen Salat mit."),
           ("Protokoll der Teamrunde", "Anbei das Protokoll. Nächste Runde am Dienstag um 10 Uhr.")],
    "es": [("Cena del viernes", "¿Quedamos el viernes para cenar? Reservo en el sitio de siempre."),
           ("Fotos de la excursión", "Te paso las fotos de la excursión al lago. ¡Qué buen día!")],
    "fr": [("Pique-nique dimanche", "On fait un pique-nique au parc dimanche midi, tu viens ?"),
           ("Notes de la réunion", "Voici les notes de la réunion de lundi. Prochaine étape : le devis.")],
}

FROM_NORMAL = ["Ada Quill <ada@quillworks.example>", "Bo Lindqvist <bo@northfjord.example>",
               "Carmen Soto <carmen@soto.example>", "Dmitri Vale <dmitri@valecraft.example>"]
FROM_JUNK = ["Deals <deals@shop-blast.example>", "Rewards <noreply@prize-centre.example>"]


def eml(frm, to, subject, body, when, msgid):
    return (f"From: {frm}\nTo: {to}\nSubject: {subject}\nDate: {format_datetime(when)}\n"
            f"Message-ID: <{msgid}@jj1.example>\nMIME-Version: 1.0\nContent-Type: text/plain; charset=utf-8\n"
            f"Content-Transfer-Encoding: 8bit\n\n{body}\n")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--n", type=int, default=2000)
    ap.add_argument("--seed", type=int, default=7)
    a = ap.parse_args()
    rnd = random.Random(a.seed)
    start = datetime(2026, 1, 5, 9, 0, tzinfo=timezone.utc)
    truth = []
    os.makedirs(os.path.join(a.out, "mail"), exist_ok=True)

    def write(folder, name, text, rec):
        d = os.path.join(a.out, "mail", folder)
        os.makedirs(d, exist_ok=True)
        p = os.path.join(d, name)
        with open(p, "w", encoding="utf-8") as f:
            f.write(text)
        rec["path"] = os.path.relpath(p, a.out)
        truth.append(rec)

    i = 0
    # Planted items go in "Inbox" (a neutral folder) so folder rules can't help.
    for lang, kind, subj, body, probe in SENSITIVE:
        when = start + timedelta(days=rnd.randrange(290), minutes=rnd.randrange(600))
        frm = rnd.choice(FROM_NORMAL)
        write("Inbox", f"s{i:04d}.eml", eml(frm, "Robin Hale <robin@hale.example>", subj, body, when, f"s{i}"),
              {"lang": lang, "class": "sensitive", "kind": kind, "probe": probe, "subject": subj})
        i += 1
    for lang, subj, body, probe in JUNK:
        for k in range(3):
            when = start + timedelta(days=rnd.randrange(290), minutes=rnd.randrange(600))
            write("Inbox", f"j{i:04d}.eml", eml(rnd.choice(FROM_JUNK), "robin@hale.example", subj, body, when, f"j{i}"),
                  {"lang": lang, "class": "junk", "kind": None, "probe": probe if k == 0 else None, "subject": subj})
            i += 1
    langs = list(NORMAL)
    while i < a.n:
        lang = rnd.choice(langs)
        subj, body = rnd.choice(NORMAL[lang])
        when = start + timedelta(days=rnd.randrange(290), minutes=rnd.randrange(600))
        tag = f"ref-{i:05d}"
        write("Inbox", f"n{i:05d}.eml", eml(rnd.choice(FROM_NORMAL), "robin@hale.example", subj,
                                              f"{body}\n\n({tag})", when, f"n{i}"),
              {"lang": lang, "class": "normal", "kind": None, "probe": tag if i % 97 == 0 else None, "subject": subj})
        i += 1
    with open(os.path.join(a.out, "truth.jsonl"), "w", encoding="utf-8") as f:
        for r in truth:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")
    print(f"wrote {len(truth)} items to {a.out}")


if __name__ == "__main__":
    main()
