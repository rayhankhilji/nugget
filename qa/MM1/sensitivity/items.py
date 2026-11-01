# MM1: FRESH sensitivity items, written for MM1 (not seen by HH2 when the rules were tuned on
# qa/GG7/scenarios/sensitive/items.py and qa/JJ1/triage). Same shape as GG7's items.py so GG7's
# run.py can measure them (run_fresh.py swaps this module in). Invented people and brands only.
# Per language: 5 sensitive situations with different vocabulary than GG7's (a different diagnosis,
# psychiatry instead of therapy, a payslip/bonus, a credit card or mortgage statement, a 2FA/PIN
# reset) and 2 ordinary mails that use look-alike words ("session", "balance", "code", "statement").
SENSITIVE = ["med", "ther", "sal", "bank", "otp"]
ORDINARY = ["dinner", "proj"]
ITEMS = {
"en": [
("med", "Riverside Imaging <results@riverside-imaging.example>", "MRI report available", "Hello Alex, the MRI of your lower back shows a herniated disc at L4-L5 pressing on the nerve root. Dr. Patel suggests physiotherapy and a review with the spine clinic in two weeks."),
("ther", "Dr. Hugh Langley <office@langley-psychiatry.example>", "Medication review", "Hi Alex, at our appointment on Monday we'll review how the sertraline is working for your depression and whether to adjust the dose."),
("sal", "Payroll <payroll@corvidworks.example>", "October payslip", "Gross pay 7,120.00, income tax 1,402.30, pension 356.00, net pay 4,980.15. Your annual bonus of 6,000.00 will be paid in December."),
("bank", "Copperline Card Services <alerts@copperline-cards.example>", "Your credit card statement", "Card ending 7731. Statement balance 2,184.66, minimum payment 65.00 due 20 November. Mortgage instalment 1,240.00 paid on 1 November."),
("otp", "Vaultly <security@vaultly.example>", "Reset your PIN", "Use 771204 to reset your PIN. Never share this code with anyone, including our staff."),
("dinner", "Tom Reid <tom.reid@example.com>", "Board games Saturday", "We still have a seat at the board game session on Saturday at 6. Bring snacks if you can."),
("proj", "Dana Reyes <dana@northwind.example>", "Code review and balance sheet template", "Please review the code for the export feature; the balance sheet template for the demo is in the shared drive. Session recordings are up too."),
],
"de": [
("med", "Radiologie am Markt <befund@radiologie-markt.example>", "MRT-Bericht", "Hallo Alex, die MRT der Lendenwirbelsäule zeigt einen Bandscheibenvorfall L4/L5 mit Wurzelkompression. Dr. Hahn empfiehlt Physiotherapie und eine Kontrolle in zwei Wochen."),
("ther", "Dr. Ingrid Vogt <praxis@vogt-psychiatrie.example>", "Termin Montag", "Hallo Alex, am Montag besprechen wir, wie das Sertralin bei Ihrer Depression wirkt und ob wir die Dosis anpassen."),
("sal", "Lohnbüro <lohn@corvidwerk.example>", "Entgeltabrechnung Oktober", "Brutto 6.120,00 €, Lohnsteuer 1.102,30 €, Sozialabgaben 1.230,00 €, Auszahlungsbetrag 3.787,70 €. Ihre Jahressonderzahlung von 4.000 € kommt im Dezember."),
("bank", "Kupferkarte Service <info@kupferkarte.example>", "Ihre Kreditkartenabrechnung", "Karte endet auf 7731. Abrechnungssaldo 2.184,66 €, Mindestzahlung 65,00 € bis 20.11. Die Hypothekenrate von 1.240,00 € wurde am 01.11. eingezogen."),
("otp", "Vaultly <sicherheit@vaultly.example>", "PIN zurücksetzen", "Mit 771204 setzen Sie Ihre PIN zurück. Geben Sie diesen Code niemals weiter, auch nicht an unsere Mitarbeiter."),
("dinner", "Tim Roth <tim.roth@example.com>", "Spieleabend Samstag", "Bei der Spielerunde am Samstag um 18 Uhr ist noch ein Platz frei. Bring gern Knabberzeug mit."),
("proj", "Dana Reyes <dana@northwind.example>", "Code-Review und Bilanzvorlage", "Bitte prüft den Code für die Exportfunktion; die Bilanzvorlage für die Demo liegt im geteilten Ordner. Die Aufzeichnungen der Sitzung sind auch online."),
],
"es": [
("med", "Clínica Resonancia Sur <informes@resonanciasur.example>", "Informe de resonancia", "Hola Alex: la resonancia lumbar muestra una hernia discal L4-L5 que comprime la raíz nerviosa. El Dr. Soto recomienda fisioterapia y revisión en dos semanas."),
("ther", "Dra. Elena Prieto <consulta@prieto-psiquiatria.example>", "Consulta del lunes", "Hola Alex: el lunes revisaremos cómo te está funcionando la sertralina para la depresión y si ajustamos la dosis."),
("sal", "Nóminas <nominas@corvidworks.example>", "Tu nómina de octubre", "Devengado 3.420,00 €, retención IRPF 512,30 €, Seguridad Social 217,00 €, líquido a percibir 2.690,70 €. La paga extra llega en diciembre."),
("bank", "Tarjetas Cobre <avisos@tarjetascobre.example>", "Liquidación de tu tarjeta de crédito", "Tarjeta terminada en 7731. Importe liquidado 2.184,66 €, pago mínimo 65,00 € antes del 20/11. Cuota de la hipoteca 1.240,00 € cargada el 01/11."),
("otp", "Vaultly <seguridad@vaultly.example>", "Restablece tu PIN", "Usa 771204 para restablecer tu PIN. No compartas este código con nadie."),
("dinner", "Tomás Ruiz <tomas.ruiz@example.com>", "Juegos de mesa el sábado", "Queda una plaza en la sesión de juegos del sábado a las 18:00. Trae algo de picar si puedes."),
("proj", "Dana Reyes <dana@northwind.example>", "Revisión de código y plantilla de balance", "Revisad el código de la exportación; la plantilla del balance para la demo está en la carpeta compartida."),
],
"fr": [
("med", "Imagerie du Parc <compte-rendu@imagerie-parc.example>", "Compte rendu d'IRM", "Bonjour Alex, l'IRM lombaire montre une hernie discale L4-L5 avec compression radiculaire. Le Dr Martin conseille de la kinésithérapie et un contrôle dans deux semaines."),
("ther", "Dr Hélène Roux <cabinet@roux-psychiatrie.example>", "Consultation de lundi", "Bonjour Alex, lundi nous ferons le point sur la sertraline pour votre dépression et sur une éventuelle adaptation de la posologie."),
("sal", "Paie <paie@corvidworks.example>", "Votre fiche de paie d'octobre", "Brut 3 820,00 €, cotisations 812,30 €, prélèvement à la source 240,00 €, net à payer 2 767,70 €. Votre prime annuelle sera versée en décembre."),
("bank", "Carte Cuivre <alertes@cartecuivre.example>", "Relevé de votre carte de crédit", "Carte se terminant par 7731. Montant débité 2 184,66 €. Mensualité de votre crédit immobilier 1 240,00 € prélevée le 01/11."),
("otp", "Vaultly <securite@vaultly.example>", "Réinitialiser votre code PIN", "Utilisez 771204 pour réinitialiser votre code PIN. Ne communiquez jamais ce code."),
("dinner", "Théo Robin <theo.robin@example.com>", "Soirée jeux samedi", "Il reste une place pour la session jeux de samedi à 18 h. Apporte de quoi grignoter si tu peux."),
("proj", "Dana Reyes <dana@northwind.example>", "Revue de code et modèle de bilan", "Merci de relire le code de l'export ; le modèle de bilan pour la démo est dans le dossier partagé."),
],
"pt-BR": [
("med", "Clínica Imagem Sul <laudos@imagemsul.example>", "Laudo da ressonância", "Olá Alex, a ressonância lombar mostra uma hérnia de disco L4-L5 comprimindo a raiz nervosa. O Dr. Lima recomenda fisioterapia e retorno em duas semanas."),
("ther", "Dra. Paula Neves <consultorio@neves-psiquiatria.example>", "Consulta de segunda", "Olá Alex, na segunda vamos avaliar como a sertralina está funcionando para a depressão e se ajustamos a dose."),
("sal", "Folha de Pagamento <folha@corvidworks.example>", "Seu holerite de outubro", "Salário bruto R$ 9.420,00, INSS R$ 908,86, IRRF R$ 1.402,30, líquido R$ 7.108,84. O décimo terceiro será pago em dezembro."),
("bank", "Cartão Cobre <avisos@cartaocobre.example>", "Fatura do seu cartão de crédito", "Cartão final 7731. Total da fatura R$ 2.184,66, pagamento mínimo R$ 65,00 até 20/11. Parcela do financiamento imobiliário R$ 1.240,00 debitada em 01/11."),
("otp", "Vaultly <seguranca@vaultly.example>", "Redefinir seu PIN", "Use 771204 para redefinir seu PIN. Nunca compartilhe este código."),
("dinner", "Tiago Rocha <tiago.rocha@example.com>", "Jogos no sábado", "Ainda tem vaga na sessão de jogos de tabuleiro no sábado às 18h. Traga petiscos se puder."),
("proj", "Dana Reyes <dana@northwind.example>", "Revisão de código e modelo de balanço", "Revisem o código da exportação; o modelo de balanço para a demo está na pasta compartilhada."),
],
"hi": [
("med", "रिवरसाइड इमेजिंग <results@riverside-imaging.example>", "एमआरआई रिपोर्ट", "नमस्ते एलेक्स, कमर की एमआरआई में L4-L5 पर स्लिप डिस्क दिखी है जो नस को दबा रही है। डॉ. पटेल ने फिजियोथेरेपी और दो हफ्ते बाद जांच की सलाह दी है।"),
("ther", "डॉ. मीरा जोशी <clinic@joshi-manochikitsa.example>", "सोमवार का अपॉइंटमेंट", "नमस्ते एलेक्स, सोमवार को हम देखेंगे कि अवसाद के लिए सर्ट्रालाइन कैसे काम कर रही है और खुराक बदलनी है या नहीं।"),
("sal", "पेरोल <payroll@corvidworks.example>", "अक्टूबर की वेतन पर्ची", "सकल वेतन ₹1,42,000, आयकर ₹18,400, पीएफ ₹9,600, शुद्ध वेतन ₹1,14,000। आपका वार्षिक बोनस दिसंबर में मिलेगा।"),
("bank", "कॉपर कार्ड <alerts@coppercard.example>", "आपके क्रेडिट कार्ड का विवरण", "कार्ड के अंतिम अंक 7731। कुल बकाया ₹21,846, न्यूनतम भुगतान ₹650, 20 नवंबर तक। होम लोन की ईएमआई ₹12,400 1 नवंबर को कटी।"),
("otp", "Vaultly <security@vaultly.example>", "अपना पिन रीसेट करें", "पिन रीसेट करने के लिए 771204 का उपयोग करें। यह कोड किसी के साथ साझा न करें।"),
("dinner", "तरुण राव <tarun.rao@example.com>", "शनिवार को बोर्ड गेम", "शनिवार शाम 6 बजे बोर्ड गेम सत्र में एक जगह खाली है। हो सके तो नाश्ता ले आना।"),
("proj", "Dana Reyes <dana@northwind.example>", "कोड समीक्षा और बैलेंस शीट टेम्पलेट", "कृपया एक्सपोर्ट फीचर का कोड देखें; डेमो के लिए बैलेंस शीट टेम्पलेट साझा फ़ोल्डर में है।"),
],
"ja": [
("med", "リバーサイド画像診断 <results@riverside-imaging.example>", "MRI検査結果のお知らせ", "アレックス様、腰椎MRIでL4-L5の椎間板ヘルニアが認められ、神経根を圧迫しています。佐藤医師よりリハビリと2週間後の再診をおすすめします。"),
("ther", "森田メンタルクリニック <clinic@morita-mental.example>", "月曜日の診察について", "アレックス様、月曜日の診察では、うつ病に対するセルトラリンの効果を確認し、用量の調整を検討します。"),
("sal", "給与担当 <kyuyo@corvidworks.example>", "10月分給与明細", "総支給額 520,000円、所得税 31,200円、社会保険料 74,800円、差引支給額 414,000円。賞与は12月に支給予定です。"),
("bank", "カッパーカード <info@coppercard.example>", "クレジットカードご利用明細", "カード末尾7731。ご請求額 218,466円、お支払日 11月26日。住宅ローン返済額 124,000円を11月1日に引き落としました。"),
("otp", "Vaultly <security@vaultly.example>", "暗証番号の再設定", "暗証番号を再設定するには771204を入力してください。このコードは誰にも教えないでください。"),
("dinner", "田村 拓 <taku.tamura@example.com>", "土曜のボードゲーム会", "土曜18時のボードゲーム会、まだ1人分空きがあります。おやつを持ってきてくれると嬉しいです。"),
("proj", "Dana Reyes <dana@northwind.example>", "コードレビューと貸借対照表テンプレート", "エクスポート機能のコードレビューをお願いします。デモ用の貸借対照表テンプレートは共有フォルダにあります。"),
],
"zh-CN": [
("med", "江畔影像中心 <results@jiangpan-imaging.example>", "核磁共振检查报告", "Alex您好，腰椎核磁共振显示L4-L5椎间盘突出，压迫神经根。王医生建议康复理疗，两周后复查。"),
("ther", "林医生心理门诊 <clinic@lin-xinli.example>", "周一复诊提醒", "Alex您好，周一复诊时我们会评估舍曲林对您抑郁症的效果，并考虑是否调整剂量。"),
("sal", "薪资组 <xinzi@corvidworks.example>", "10月工资条", "应发工资 32,000元，个人所得税 2,140元，社保公积金 5,760元，实发工资 24,100元。年终奖将于12月发放。"),
("bank", "铜卡中心 <notice@tongka.example>", "信用卡账单", "尾号7731的信用卡本期应还 21,846.60元，最低还款 650元，还款日11月20日。房贷月供 12,400元已于11月1日扣款。"),
("otp", "Vaultly <security@vaultly.example>", "重置您的密码", "请使用771204重置您的支付密码。请勿将此码告诉任何人。"),
("dinner", "唐磊 <tang.lei@example.com>", "周六桌游", "周六晚上6点的桌游局还有一个位置，有空带点零食过来。"),
("proj", "Dana Reyes <dana@northwind.example>", "代码评审和资产负债表模板", "请评审导出功能的代码；演示用的资产负债表模板在共享文件夹里。"),
],
"ar": [
("med", "مركز الضفة للأشعة <results@aldiffa-imaging.example>", "تقرير الرنين المغناطيسي", "مرحباً أليكس، أظهر الرنين المغناطيسي للفقرات القطنية انزلاقاً غضروفياً بين L4 وL5 يضغط على جذر العصب. ينصح الدكتور سامي بالعلاج الطبيعي ومراجعة بعد أسبوعين."),
("ther", "د. ليلى حداد <clinic@haddad-psych.example>", "موعد يوم الاثنين", "مرحباً أليكس، سنراجع يوم الاثنين مدى فعالية السيرترالين في علاج الاكتئاب وما إذا كنا سنعدل الجرعة."),
("sal", "قسم الرواتب <payroll@corvidworks.example>", "كشف راتب أكتوبر", "إجمالي الراتب 18,400 ريال، الاستقطاعات 1,380 ريال، صافي الراتب 17,020 ريال. تصرف المكافأة السنوية في ديسمبر."),
("bank", "بطاقات النحاس <alerts@nuhas-cards.example>", "كشف حساب بطاقتك الائتمانية", "البطاقة المنتهية بـ 7731. المبلغ المستحق 2,184.66 ريال، الحد الأدنى للسداد 65 ريال قبل 20 نوفمبر. تم خصم قسط التمويل العقاري 1,240 ريال في 1 نوفمبر."),
("otp", "Vaultly <security@vaultly.example>", "إعادة تعيين الرقم السري", "استخدم 771204 لإعادة تعيين رقمك السري. لا تشارك هذا الرمز مع أي شخص."),
("dinner", "طارق رشيد <tariq.rashid@example.com>", "ألعاب السبت", "ما زال هناك مكان في جلسة ألعاب الطاولة يوم السبت الساعة السادسة. أحضر بعض الوجبات الخفيفة إن استطعت."),
("proj", "Dana Reyes <dana@northwind.example>", "مراجعة الشيفرة وقالب الميزانية", "يرجى مراجعة شيفرة ميزة التصدير؛ قالب الميزانية العمومية للعرض موجود في المجلد المشترك."),
],
}
