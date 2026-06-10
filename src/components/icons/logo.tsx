export const Logo = (props: React.SVGProps<SVGSVGElement>) => (
  <svg
    xmlns="http://www.w3.org/2000/svg"
    role="graphics-symbol img"
    fill="currentColor"
    {...props}
    viewBox="0 0 44 44"
    width="44"
    height="44"
  >
    <title>Watermelon tray icon</title>
    <rect
      x="2"
      y="2"
      width="40"
      height="40"
      rx="10"
      fill="#fff"
      stroke="#000"
      strokeWidth="2"
    />
    <circle
      cx="20"
      cy="22"
      r="12"
      fill="#67bd45"
      stroke="#000"
      strokeWidth="3"
    />
    <path
      d="M13 13c2 5 2 12 0 18M20 10c2 7 2 17 0 24M27 13c2 5 2 12 0 18"
      fill="none"
      stroke="#0d6930"
      strokeWidth="3"
      strokeLinecap="round"
    />
    <path
      d="M14 27c7 7 20 5 25-5-8-1-17 1-25 5z"
      fill="#f52d2d"
      stroke="#000"
      strokeWidth="3"
      strokeLinejoin="round"
    />
    <path
      d="M17 28c6 4 14 3 19-2"
      fill="none"
      stroke="#fff"
      strokeWidth="2.5"
      strokeLinecap="round"
    />
    <ellipse
      cx="25"
      cy="27"
      rx="1.3"
      ry="2.2"
      fill="#000"
      transform="rotate(-20 25 27)"
    />
    <ellipse
      cx="31"
      cy="25"
      rx="1.3"
      ry="2.2"
      fill="#000"
      transform="rotate(-35 31 25)"
    />
  </svg>
);
