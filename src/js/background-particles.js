/**
 * Fluence — Premium Immersive Background Particles
 * High-performance 3D Constellation Network with Parallax & Repulsion
 */

(function () {
  // Respect user preference for reduced motion
  const motionQuery = window.matchMedia('(prefers-reduced-motion: reduce)');
  let isReducedMotion = motionQuery.matches;

  motionQuery.addEventListener('change', (e) => {
    isReducedMotion = e.matches;
    if (isReducedMotion) {
      if (window.networkInstance) window.networkInstance.stop();
    } else {
      if (window.networkInstance) window.networkInstance.start();
    }
  });

  // Check if touch/gyroscope is supported (typically mobile)
  const isTouchDevice = 'ontouchstart' in window || navigator.maxTouchPoints > 0;

  class ParticleNetwork {
    constructor(canvasId) {
      this.canvas = document.getElementById(canvasId);
      if (!this.canvas) return;

      this.ctx = this.canvas.getContext('2d');
      this.particles = [];
      this.animationFrameId = null;

      // Mouse/Interaction state
      this.mouse = {
        x: null,
        y: null,
        targetX: null,
        targetY: null,
        active: false,
        radius: 130, // Repulsion radius
      };

      // Global Parallax state
      this.parallax = {
        x: 0,
        y: 0,
        targetX: 0,
        targetY: 0,
        ease: 0.08,
      };

      // Configuration
      this.config = {
        maxParticles: isTouchDevice ? 32 : 75,
        connectionDistance: 110,
        baseSpeed: 0.12,
        repulsionForce: 0.45,
        restoreSpeed: 0.08,
      };

      this.init();
    }

    init() {
      this.resize();
      this.createParticles();
      this.setupEventListeners();
      this.start();
    }

    resize() {
      this.pixelRatio = window.devicePixelRatio || 1;
      this.width = window.innerWidth;
      this.height = window.innerHeight;

      // Scale canvas for high-DPI displays
      this.canvas.width = this.width * this.pixelRatio;
      this.canvas.height = this.height * this.pixelRatio;
      this.canvas.style.width = `${this.width}px`;
      this.canvas.style.height = `${this.height}px`;
      this.ctx.scale(this.pixelRatio, this.pixelRatio);

      // Re-bound particles if coordinates are out of bounds
      this.particles.forEach(p => {
        if (p.x > this.width) p.x = Math.random() * this.width;
        if (p.y > this.height) p.y = Math.random() * this.height;
      });
    }

    createParticles() {
      this.particles = [];
      for (let i = 0; i < this.config.maxParticles; i++) {
        // z-depth simulated from 0.5 (far, slow, dim) to 2.0 (close, fast, bright)
        const z = 0.5 + Math.random() * 1.5;
        const speedMultiplier = z * this.config.baseSpeed;
        const angle = Math.random() * Math.PI * 2;

        // Brand colors (purple/cyan) blend
        const isPurple = Math.random() > 0.4;
        const color = isPurple ? '168, 85, 247' : '0, 242, 254';

        this.particles.push({
          x: Math.random() * this.width,
          y: Math.random() * this.height,
          vx: Math.cos(angle) * speedMultiplier,
          vy: Math.sin(angle) * speedMultiplier,
          z: z,
          radius: (z / 2.0) * 2.2 + 0.5, // sizes between 1px and 2.7px
          alpha: z / 2.2 * 0.45 + 0.15,  // opacity between 0.25 and 0.6
          color: color,
          // Displacement for cursor repulsion
          dispX: 0,
          dispY: 0,
        });
      }
    }

    setupEventListeners() {
      // Mouse move
      const handleMouseMove = (e) => {
        this.mouse.active = true;
        this.mouse.targetX = e.clientX;
        this.mouse.targetY = e.clientY;

        // Target global parallax based on cursor offset from center
        this.parallax.targetX = (e.clientX - this.width / 2) * 0.04;
        this.parallax.targetY = (e.clientY - this.height / 2) * 0.04;
      };

      // Mouse leave
      const handleMouseLeave = () => {
        this.mouse.active = false;
        this.mouse.targetX = null;
        this.mouse.targetY = null;
        this.parallax.targetX = 0;
        this.parallax.targetY = 0;
      };

      window.addEventListener('mousemove', handleMouseMove);
      document.addEventListener('mouseleave', handleMouseLeave);
      window.addEventListener('resize', () => this.resize());

      // Device Gyroscope support for mobile views
      if (isTouchDevice && window.DeviceOrientationEvent) {
        let lastBeta = 0;
        let lastGamma = 0;

        window.addEventListener('deviceorientation', (e) => {
          if (isReducedMotion) return;

          // gamma: left-to-right tilt (-90 to 90)
          // beta: front-to-back tilt (-180 to 180)
          const tiltX = e.gamma ? e.gamma : 0;
          const tiltY = e.beta ? e.beta : 0;

          // Low pass filter to avoid high-frequency jitter
          lastBeta = lastBeta + (tiltY - lastBeta) * 0.1;
          lastGamma = lastGamma + (tiltX - lastGamma) * 0.1;

          // Scale orientation angles to parallax shift
          this.parallax.targetX = lastGamma * 0.8;
          this.parallax.targetY = (lastBeta - 45) * 0.8; // Assume holding at 45 degree angle
        });
      }
    }

    update() {
      // Smoothly interpolate global parallax
      this.parallax.x += (this.parallax.targetX - this.parallax.x) * this.parallax.ease;
      this.parallax.y += (this.parallax.targetY - this.parallax.y) * this.parallax.ease;

      // Smoothly interpolate mouse coordinate trackers
      if (this.mouse.targetX !== null && this.mouse.targetY !== null) {
        if (this.mouse.x === null) {
          this.mouse.x = this.mouse.targetX;
          this.mouse.y = this.mouse.targetY;
        } else {
          this.mouse.x += (this.mouse.targetX - this.mouse.x) * 0.15;
          this.mouse.y += (this.mouse.targetY - this.mouse.y) * 0.15;
        }
      } else {
        this.mouse.x = null;
        this.mouse.y = null;
      }

      // Update particle positions
      this.particles.forEach(p => {
        // Base drift motion
        p.x += p.vx;
        p.y += p.vy;

        // Wrap around screen boundaries
        if (p.x < -20) p.x = this.width + 20;
        if (p.x > this.width + 20) p.x = -20;
        if (p.y < -20) p.y = this.height + 20;
        if (p.y > this.height + 20) p.y = -20;

        // Calculate cursor repulsion displacement
        let targetDispX = 0;
        let targetDispY = 0;

        if (this.mouse.active && this.mouse.x !== null) {
          // Include current parallax shift in distance calculations
          const currentRenderX = p.x + this.parallax.x * (p.z / 2.0);
          const currentRenderY = p.y + this.parallax.y * (p.z / 2.0);

          const dx = currentRenderX - this.mouse.x;
          const dy = currentRenderY - this.mouse.y;
          const distance = Math.hypot(dx, dy);

          if (distance < this.mouse.radius && distance > 0) {
            const force = (this.mouse.radius - distance) / this.mouse.radius;
            // Push away proportional to force and particle's depth (foreground repels more)
            const push = force * 45 * (p.z / 1.5) * this.config.repulsionForce;
            targetDispX = (dx / distance) * push;
            targetDispY = (dy / distance) * push;
          }
        }

        // Smoothly interpolate the displacement forces (spring effect)
        p.dispX += (targetDispX - p.dispX) * this.config.restoreSpeed;
        p.dispY += (targetDispY - p.dispY) * this.config.restoreSpeed;
      });
    }

    draw() {
      this.ctx.clearRect(0, 0, this.width, this.height);

      // 1. Draw Constellation lines
      const maxDistance = this.config.connectionDistance;
      const count = this.particles.length;

      for (let i = 0; i < count; i++) {
        const p1 = this.particles[i];
        const x1 = p1.x + p1.dispX + this.parallax.x * (p1.z / 2.0);
        const y1 = p1.y + p1.dispY + this.parallax.y * (p1.z / 2.0);

        for (let j = i + 1; j < count; j++) {
          const p2 = this.particles[j];
          
          // Skip if depth differences are too large (adds 3D separation)
          if (Math.abs(p1.z - p2.z) > 0.6) continue;

          const x2 = p2.x + p2.dispX + this.parallax.x * (p2.z / 2.0);
          const y2 = p2.y + p2.dispY + this.parallax.y * (p2.z / 2.0);

          const dx = x1 - x2;
          const dy = y1 - y2;
          const distance = Math.hypot(dx, dy);

          if (distance < maxDistance) {
            const fraction = (maxDistance - distance) / maxDistance;
            // Lines are fainter than particles, scaling down to max opacity of 0.14
            const avgAlpha = (p1.alpha + p2.alpha) / 2.0;
            const lineAlpha = fraction * avgAlpha * 0.14; 

            this.ctx.beginPath();
            this.ctx.moveTo(x1, y1);
            this.ctx.lineTo(x2, y2);
            
            // Draw gradient lines or brand blending lines
            // We use simple solid purple-tinted color style for maximum speed
            this.ctx.strokeStyle = `rgba(180, 140, 255, ${lineAlpha})`;
            this.ctx.lineWidth = fraction * 0.8;
            this.ctx.stroke();
          }
        }
      }

      // 2. Draw Particles
      this.particles.forEach(p => {
        // Calculate rendering position: base drift + repulsion displacement + global parallax shift
        const renderX = p.x + p.dispX + this.parallax.x * (p.z / 2.0);
        const renderY = p.y + p.dispY + this.parallax.y * (p.z / 2.0);

        this.ctx.beginPath();
        this.ctx.arc(renderX, renderY, p.radius, 0, Math.PI * 2);
        this.ctx.fillStyle = `rgba(${p.color}, ${p.alpha})`;
        
        // Add subtle glow to foreground elements
        if (p.z > 1.4) {
          this.ctx.shadowColor = `rgba(${p.color}, ${p.alpha * 0.8})`;
          this.ctx.shadowBlur = 4;
        } else {
          this.ctx.shadowBlur = 0;
        }

        this.ctx.fill();
      });

      // Reset shadow blur
      this.ctx.shadowBlur = 0;
    }

    tick() {
      if (isReducedMotion) return;

      this.update();
      this.draw();
      this.animationFrameId = requestAnimationFrame(() => this.tick());
    }

    start() {
      if (isReducedMotion) {
        // For reduced motion, just draw a single static frame and don't animate
        this.draw();
        return;
      }
      if (!this.animationFrameId) {
        this.tick();
      }
    }

    stop() {
      if (this.animationFrameId) {
        cancelAnimationFrame(this.animationFrameId);
        this.animationFrameId = null;
      }
    }
  }

  // Auto-initialize when the DOM is ready
  window.addEventListener('DOMContentLoaded', () => {
    window.networkInstance = new ParticleNetwork('background-canvas');
  });
})();
