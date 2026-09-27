import "./globals.css";

const site = process.env.VERCEL_PROJECT_PRODUCTION_URL;

export const metadata = {
  metadataBase: new URL(site ? `https://${site}` : "http://localhost:3000"),
  title: "RyzikOS — операционная система на Rust",
  description:
    "RyzikOS — любительская операционная система для x86_64 на ассемблере и Rust: рабочий стол, браузер, App Store, файлы, звук. Скачайте последнюю версию.",
  icons: { icon: "/logo.png" },
  openGraph: {
    title: "RyzikOS",
    description: "Операционная система с нуля на ассемблере и Rust.",
    images: ["/shots/desktop-icons.jpg"],
  },
};

export const viewport = {
  themeColor: "#000000",
};

export default function RootLayout({ children }) {
  return (
    <html lang="ru">
      <body>{children}</body>
    </html>
  );
}
