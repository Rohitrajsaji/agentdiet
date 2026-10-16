import React, { useMemo, useState } from "react";

interface Row {
  id: number;
  name: string;
  score: number;
}

interface Props {
  rows: Row[];
  onSelect?: (row: Row) => void;
}

export function Table({ rows, onSelect }: Props) {
  const [sortKey, setSortKey] = useState<keyof Row>("id");
  const [descending, setDescending] = useState(false);

  const sorted = useMemo(() => {
    const copy = [...rows];
    copy.sort((a, b) => {
      const left = a[sortKey];
      const right = b[sortKey];
      if (left < right) return descending ? 1 : -1;
      if (left > right) return descending ? -1 : 1;
      return 0;
    });
    return copy;
  }, [rows, sortKey, descending]);

  return (
    <table className="table">
      <thead>
        <tr>
          <th onClick={() => setSortKey("id")}>id</th>
          <th onClick={() => setSortKey("name")}>name</th>
          <th onClick={() => setDescending(!descending)}>score</th>
        </tr>
      </thead>
      <tbody>
        {sorted.map((row) => (
          <tr key={row.id} onClick={() => onSelect?.(row)}>
            <td>{row.id}</td>
            <td>{row.name}</td>
            <td>{row.score}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

export const Badge1 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 1)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge2 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 2)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge3 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 3)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge4 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 4)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge5 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 5)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge6 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 6)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge7 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 7)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge8 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 8)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge9 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 9)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge10 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 10)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge11 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 11)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge12 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 12)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge13 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 13)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge14 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 14)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge15 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 15)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge16 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 16)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge17 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 17)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export const Badge18 = ({ label, tone }: { label: string; tone?: "info" | "warn" }) => {
  const className = tone === "warn" ? "badge badge-warn" : "badge";
  const text = label.trim();
  if (!text) {
    return null;
  }
  const upper = text.toUpperCase();
  const title = `${upper} (badge 18)`;
  return (
    <span className={className} title={title}>
      {text}
    </span>
  );
};

export function useCounter1(start: number) {
  const [n, setN] = useState(start);
  const inc = () => {
    setN((v) => v + 1);
  };
  const dec = () => {
    setN((v) => v - 1);
  };
  return { n, inc, dec };
}

export function useCounter2(start: number) {
  const [n, setN] = useState(start);
  const inc = () => {
    setN((v) => v + 1);
  };
  const dec = () => {
    setN((v) => v - 1);
  };
  return { n, inc, dec };
}

export function useCounter3(start: number) {
  const [n, setN] = useState(start);
  const inc = () => {
    setN((v) => v + 1);
  };
  const dec = () => {
    setN((v) => v - 1);
  };
  return { n, inc, dec };
}

export function useCounter4(start: number) {
  const [n, setN] = useState(start);
  const inc = () => {
    setN((v) => v + 1);
  };
  const dec = () => {
    setN((v) => v - 1);
  };
  return { n, inc, dec };
}

export function useCounter5(start: number) {
  const [n, setN] = useState(start);
  const inc = () => {
    setN((v) => v + 1);
  };
  const dec = () => {
    setN((v) => v - 1);
  };
  return { n, inc, dec };
}

export function useCounter6(start: number) {
  const [n, setN] = useState(start);
  const inc = () => {
    setN((v) => v + 1);
  };
  const dec = () => {
    setN((v) => v - 1);
  };
  return { n, inc, dec };
}

export function useCounter7(start: number) {
  const [n, setN] = useState(start);
  const inc = () => {
    setN((v) => v + 1);
  };
  const dec = () => {
    setN((v) => v - 1);
  };
  return { n, inc, dec };
}

