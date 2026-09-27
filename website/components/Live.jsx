"use client";

import { useEffect, useState } from "react";

/** The menu bar clock and date, like the system's. */
export function Clock() {
  const [now, setNow] = useState(null);
  useEffect(() => {
    setNow(new Date());
    const t = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(t);
  }, []);
  if (!now) return <b>--:--</b>;
  const pad = (n) => String(n).padStart(2, "0");
  return (
    <>
      <span className="dim hide-sm">
        {pad(now.getDate())}.{pad(now.getMonth() + 1)}.{now.getFullYear()}
      </span>
      <b>
        {pad(now.getHours())}:{pad(now.getMinutes())}
      </b>
    </>
  );
}

/** Fade sections in as they scroll into view. */
export function Reveal() {
  useEffect(() => {
    const items = document.querySelectorAll(".reveal");
    if (!("IntersectionObserver" in window)) {
      items.forEach((el) => el.classList.add("shown"));
      return;
    }
    const io = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) {
            e.target.classList.add("shown");
            io.unobserve(e.target);
          }
        }
      },
      { threshold: 0.12 }
    );
    items.forEach((el) => io.observe(el));
    return () => io.disconnect();
  }, []);
  return null;
}

/** The terminal window types its commands out. */
export function Typing({ lines }) {
  const [shown, setShown] = useState(0);
  useEffect(() => {
    const total = lines.reduce((n, l) => n + l.text.length, 0);
    if (shown >= total) return;
    const t = setTimeout(() => setShown((s) => s + 1), shown === 0 ? 600 : 18);
    return () => clearTimeout(t);
  }, [shown, lines]);
  let left = shown;
  return (
    <>
      {lines.map((l, i) => {
        const part = l.text.slice(0, Math.max(0, left));
        left -= l.text.length;
        return (
          <span key={i} className={l.cls}>
            {part}
          </span>
        );
      })}
      <span className="caret" />
    </>
  );
}
