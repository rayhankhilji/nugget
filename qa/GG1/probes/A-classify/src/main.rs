//! GG1 area A probe (synthetic inputs only). Prints what nugget-a11y makes of quoted mail in
//! several languages and of the AT-SPI "terminal" role.
use nugget_a11y::role::role_from_atspi;
use nugget_a11y::thread::quoted_messages;

fn show(label: &str, value: &str) {
    let m = quoted_messages(value);
    println!("== {label}: {} message(s)", m.len());
    for x in &m {
        println!("   author={:?} ts={:?} text[..80]={:?}", x.author, x.timestamp,
            x.text.chars().take(80).collect::<String>());
    }
}

fn main() {
    show("en gmail", "Sounds good.\n\nOn Thu, 29 Oct 2026 at 10:00, Lena Park <lena.park@venue.example.com> wrote:\n> Can you confirm the deposit?\n");
    show("es gmail", "Perfecto.\n\nEl jue, 29 oct 2026 a las 10:00, Lucía Martín <lucia.martin@venue.example.com> escribió:\n> ¿Me confirmas el importe del depósito?\n");
    show("en outlook", "Thanks!\n\nFrom: Lena Park <lena.park@venue.example.com>\nSent: Thursday, October 29, 2026 10:00 AM\nTo: Pat Okafor\nSubject: Deposit\n\nCan you confirm the deposit?\n");
    show("de outlook", "Danke!\n\nVon: Lena Park <lena.park@venue.example.com>\nGesendet: Donnerstag, 29. Oktober 2026 10:00\nAn: Pat Okafor\nBetreff: Anzahlung\n\nKönnen Sie die Anzahlung bestätigen?\n");
    show("nested en", "Ok.\n\nOn Thu, 29 Oct 2026, Omar Haddad <omar.haddad@northwind.example.com> wrote:\n> What is the deposit amount?\n>\n> On Mon, 12 Oct 2026, Lena Park <lena.park@venue.example.com> wrote:\n>> Contract attached. Parking is free.\n");
    println!("== AT-SPI role 'terminal' -> {:?}", role_from_atspi("terminal", None));
    println!("== AT-SPI role 'password text' -> {:?}", role_from_atspi("password text", None));
}
