/** @type {import('tailwindcss').Config} */
export default {
  content: [
    "./index.html",
    "./src/**/*.{js,ts,jsx,tsx}",
  ],
  theme: {
    extend: {
      colors: {
        canvas: '#F4F4F5',
        card: '#FFFFFF',
        border: '#E4E4E7',
        brand: {
          dark: '#18181B',
          muted: '#71717A',
          subtle: '#A1A1AA',
          green: '#16A34A',
          amber: '#92400E',
          red: '#DC2626',
          blue: '#2563EB',
        }
      },
      borderRadius: {
        'card': '20px',
        'menu': '16px',
        'pill': '9999px',
      },
      boxShadow: {
        'card': '0 1px 3px 0 rgba(0, 0, 0, 0.05), 0 1px 2px -1px rgba(0, 0, 0, 0.05)',
        'card-hover': '0 4px 6px -1px rgba(0, 0, 0, 0.05), 0 2px 4px -2px rgba(0, 0, 0, 0.05)',
        'modal': '0 20px 25px -5px rgba(0, 0, 0, 0.1), 0 8px 10px -6px rgba(0, 0, 0, 0.1)',
      }
    },
  },
  plugins: [],
}
