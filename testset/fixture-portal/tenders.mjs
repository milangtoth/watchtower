// Fictional tenders for the fixture portal. Buyers, references and documents are invented.
// Dates are relative to the build day so the fixture always has open tenders and one closed one.

const DAY = 24 * 60 * 60 * 1000;

/** Formats a UTC-based calendar date as a date-time with an explicit +01:00 offset. */
function at(date, hour) {
  const ymd = date.toISOString().slice(0, 10);
  return `${ymd}T${String(hour).padStart(2, "0")}:00:00+01:00`;
}

export function fixtureTenders(today = new Date()) {
  const base = new Date(Date.UTC(today.getUTCFullYear(), today.getUTCMonth(), today.getUTCDate()));
  const days = n => new Date(base.getTime() + n * DAY);
  // The 10th of next month, so "closing next month" has a German IT match.
  const nextMonth10 = new Date(Date.UTC(base.getUTCFullYear(), base.getUTCMonth() + 1, 10));
  const date = n => days(n).toISOString().slice(0, 10);

  return [
    {
      ref: "NL-2026-1041", title: "IT-ondersteuning en werkplekbeheer gemeentekantoren", buyer: "Gemeente Lindenhaven",
      country: "NL", city: "Lindenhaven", postal: "3511 AB", cpv: "72253000", published: date(-17), deadline: at(days(25), 12),
      procedure: ["Openbaar", "open"], nature: ["Diensten", "services"],
      description: "De gemeente zoekt een partner voor eerste- en tweedelijns IT-ondersteuning, beheer van circa 900 werkplekken en een servicedesk op werkdagen van 8.00 tot 18.00 uur. Inschrijvers tonen ISO 27001-certificering en minimaal drie vergelijkbare referenties aan. Looptijd vier jaar met twee optionele verlengingen van een jaar.",
      docs: ["bestek-werkplekbeheer.pdf", "nota-van-inlichtingen-1.pdf"],
    },
    {
      ref: "NL-2026-1057", title: "Servicedesk en IT support voor waterschapslocaties", buyer: "Waterschap Rijnmond-Oost",
      country: "NL", city: "Dordwijk", cpv: "72611000", published: date(-11), deadline: at(days(14), 10),
      procedure: ["Niet-openbaar", "restricted"], nature: ["Diensten", "services"],
      description: "Levering van een servicedesk en IT-support op locatie voor zes kantoren en twee gemalen. Selectiefase op basis van geschiktheidseisen; maximaal vijf gegadigden worden uitgenodigd.",
      docs: ["selectieleidraad.pdf"],
    },
    {
      ref: "DE-2026-2210", title: "IT-Support und Anwenderbetreuung für Schulen", buyer: "Stadt Bergfeld",
      country: "DE", city: "Bergfeld", postal: "34117", cpv: "72611000", published: date(-14), deadline: at(nextMonth10, 10),
      procedure: ["Offenes Verfahren", "open"], nature: ["Dienstleistungen", "services"],
      description: "Technischer IT-Support und Anwenderbetreuung für 14 Schulen mit rund 3.200 Endgeräten, einschließlich Vor-Ort-Einsätzen innerhalb von vier Stunden. Nachweis einer Zertifizierung nach ISO 20000 erforderlich. Vertragslaufzeit drei Jahre.",
      docs: ["leistungsbeschreibung.pdf", "vergabeunterlagen.pdf"],
    },
    {
      ref: "DE-2026-2234", title: "Wartung der Netzwerkinfrastruktur im Kreishaus", buyer: "Landkreis Ostheide",
      country: "DE", city: "Ostheide", cpv: "72700000", published: date(-8), deadline: at(days(61), 12),
      procedure: ["Verhandlungsverfahren", "negotiated"], nature: ["Dienstleistungen", "services"],
      description: "Wartung und Störungsbeseitigung der aktiven Netzwerkkomponenten (Switches, WLAN, Firewalls) im Kreishaus und in vier Außenstellen.",
      docs: ["leistungsverzeichnis.pdf"],
    },
    {
      ref: "NL-2026-1063", title: "Groot onderhoud fietspaden", buyer: "Provincie Noordwaard",
      country: "NL", city: "Noordwaard", cpv: "45233162", published: date(-10), deadline: at(days(32), 14),
      procedure: ["Openbaar", "open"], nature: ["Werken", "works"],
      description: "Herstel en herprofilering van circa 18 kilometer provinciale fietspaden, inclusief verkeersmaatregelen.",
      docs: ["bestek-fietspaden.pdf"],
    },
    {
      ref: "BE-2026-0310", title: "Support informatique pour les services communaux", buyer: "Commune de Valbourg",
      country: "BE", city: "Valbourg", postal: "5000", cpv: "72611000", published: date(-6), deadline: at(days(28), 11),
      procedure: ["Procédure ouverte", "open"], nature: ["Services", "services"],
      description: "Support informatique de premier niveau pour l'administration communale et la bibliothèque, environ 150 postes de travail. Intervention sur site dans un délai d'un jour ouvrable.",
      docs: ["cahier-des-charges.pdf"],
    },
    {
      // The notice states no buyer: Watchtower must publish buyerName as null, not the placeholder text.
      ref: "DE-2026-2251", title: "Unterhaltsreinigung von Verwaltungsgebäuden", buyer: null,
      country: "DE", city: "Bergfeld", cpv: "90911200", published: date(-5), deadline: at(days(15), 9),
      procedure: ["Offenes Verfahren", "open"], nature: ["Dienstleistungen", "services"],
      description: "Unterhaltsreinigung von drei Verwaltungsgebäuden mit insgesamt 12.000 m² Reinigungsfläche.",
      docs: [],
    },
    {
      // Its deadline has passed: Watchtower must publish it as closed.
      ref: "NL-2026-0990", title: "Levering van laptops voor onderwijsinstellingen", buyer: "Stichting Openbaar Onderwijs Lindenhaven",
      country: "NL", city: "Lindenhaven", cpv: "30213100", published: date(-50), deadline: at(days(-19), 12),
      procedure: ["Openbaar", "open"], nature: ["Leveringen", "supplies"],
      description: "Levering van 1.200 laptops inclusief vijf jaar garantie.",
      docs: ["programma-van-eisen.pdf"],
    },
  ];
}

export const slug = tender => tender.ref.toLowerCase();
